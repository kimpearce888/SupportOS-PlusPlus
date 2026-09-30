//! Activity engine — derives timestamps + response states from conversation changes.
//!
//! Per spec M3: "events, derived timestamps, response states."
//! Per KNOWN PITFALLS: "Never compare ISO-8601 timestamps against SQLite
//! datetime('now') strings lexically; store one format or compare via
//! julianday/unixepoch."
//! Per KNOWN PITFALLS: "Dedup keys on every derived event so rebuilds and
//! re-syncs are idempotent."
//! Per KNOWN PITFALLS: "Reads have no side effects; rebuilds only via
//! explicit commands."

use rusqlite::{params, Connection};

use crate::catalog::ResponseState;
use crate::error::Result;

/// An activity event — a record of something that happened to a conversation.
/// The engine derives these from conversation changes (sync writes + webhook events).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivityEvent {
    pub id: Option<i64>,
    pub conversation_id: i64,
    pub event_type: String,
    pub actor_type: String,
    pub actor_id: Option<i64>,
    pub occurred_at: String,
    pub dedup_key: String,
}

/// The M003 migration: creates the `activity_events` table + indexes.
pub const M003_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS activity_events (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        event_type      TEXT NOT NULL,
        actor_type      TEXT NOT NULL,
        actor_id        INTEGER,
        occurred_at     TEXT NOT NULL,
        dedup_key        TEXT NOT NULL UNIQUE,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_activity_events_conv
        ON activity_events (conversation_id, occurred_at);
    CREATE INDEX IF NOT EXISTS idx_activity_events_type
        ON activity_events (event_type, occurred_at);

    -- Derived columns on conversations (updated by the activity engine).
    -- These are the "derived timestamps" the spec requires:
    -- first_customer_message_at, first_response_at, last_customer_reply_at,
    -- last_human_agent_response_at, last_activity_at, closed_at,
    -- customer_waiting_since, response_state.
    -- All added as nullable columns so existing rows don't break.
    ALTER TABLE conversations ADD COLUMN first_customer_message_at TEXT;
    ALTER TABLE conversations ADD COLUMN first_response_at TEXT;
    ALTER TABLE conversations ADD COLUMN last_customer_reply_at TEXT;
    ALTER TABLE conversations ADD COLUMN last_human_agent_response_at TEXT;
    ALTER TABLE conversations ADD COLUMN customer_waiting_since TEXT;
    ALTER TABLE conversations ADD COLUMN response_state TEXT NOT NULL DEFAULT 'needs_first_response';

    UPDATE app_state SET schema_version = 3 WHERE id = 1;
"#;

/// Apply M003 migration. Idempotent (uses IF NOT EXISTS for tables + indexes;
/// ALTER TABLE ADD COLUMN is SQLite-native idempotent-safe via try/catch).
pub fn apply_m003(conn: &Connection) -> Result<()> {
    // Split the SQL into statements so ALTER TABLE failures (column already
    // exists) don't abort the whole migration.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS activity_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL,
            event_type      TEXT NOT NULL,
            actor_type      TEXT NOT NULL,
            actor_id        INTEGER,
            occurred_at     TEXT NOT NULL,
            dedup_key        TEXT NOT NULL UNIQUE,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_activity_events_conv
            ON activity_events (conversation_id, occurred_at);
        CREATE INDEX IF NOT EXISTS idx_activity_events_type
            ON activity_events (event_type, occurred_at);",
    )?;

    // ALTER TABLE ADD COLUMN — each is idempotent-safe via try/catch.
    for col in [
        "first_customer_message_at TEXT",
        "first_response_at TEXT",
        "last_customer_reply_at TEXT",
        "last_human_agent_response_at TEXT",
        "customer_waiting_since TEXT",
        "response_state TEXT NOT NULL DEFAULT 'needs_first_response'",
    ] {
        let sql = format!("ALTER TABLE conversations ADD COLUMN {col}");
        // Ignore "duplicate column name" error (column already exists).
        if let Err(e) = conn.execute(&sql, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(crate::error::Error::Sqlite(e));
            }
        }
    }

    // Update schema_version.
    conn.execute("UPDATE app_state SET schema_version = 3 WHERE id = 1", [])?;

    Ok(())
}

/// Record an activity event. Idempotent: if the `dedup_key` already exists,
/// the event is silently skipped (no error, no duplicate row).
pub fn record_event(conn: &Connection, event: &ActivityEvent) -> Result<bool> {
    let rows = conn.execute(
        "INSERT OR IGNORE INTO activity_events
            (conversation_id, event_type, actor_type, actor_id, occurred_at, dedup_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.conversation_id,
            event.event_type,
            event.actor_type,
            event.actor_id,
            event.occurred_at,
            event.dedup_key,
        ],
    )?;
    Ok(rows > 0)
}

/// Derive the response state for a conversation from its activity events.
///
/// The 4 states (from the catalog `ResponseState` enum):
/// - `needs_first_response`: no agent has replied yet.
/// - `customer_waiting`: customer sent the last message; agent hasn't replied.
/// - `agent_waiting`: agent sent the last message; waiting for customer.
/// - `closed`: conversation is closed.
///
/// This function reads the conversation + its events and computes the state.
/// It does NOT write — the caller must call `update_response_state` to persist.
pub fn derive_response_state(
    conn: &Connection,
    conversation_remote_id: i64,
) -> Result<ResponseState> {
    // Check if the conversation is closed.
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM conversations WHERE remote_id = ?1",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    if status.as_deref() == Some("closed") {
        return Ok(ResponseState::Closed);
    }

    // Get the last activity event for this conversation.
    let last_event: Option<(String, String)> = conn
        .query_row(
            "SELECT event_type, actor_type FROM activity_events
             WHERE conversation_id = ?1
             ORDER BY julianday(occurred_at) DESC
             LIMIT 1",
            params![conversation_remote_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();

    match last_event {
        None => Ok(ResponseState::NeedsFirstResponse),
        Some((_event_type, actor_type)) => {
            if actor_type == "customer" {
                Ok(ResponseState::CustomerWaiting)
            } else if actor_type == "agent" {
                Ok(ResponseState::AgentWaiting)
            } else {
                // System events don't change the response state.
                Ok(ResponseState::NeedsFirstResponse)
            }
        }
    }
}

/// Update the derived columns on a conversation based on its activity events.
/// This is the "rebuild" function — called after a sync or webhook event.
pub fn update_derived_columns(conn: &Connection, conversation_remote_id: i64) -> Result<()> {
    let state = derive_response_state(conn, conversation_remote_id)?;

    // Compute derived timestamps from events.
    let first_customer_message_at: Option<String> = conn
        .query_row(
            "SELECT MIN(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type = 'customer'",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    let first_response_at: Option<String> = conn
        .query_row(
            "SELECT MIN(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type = 'agent' AND event_type = 'reply'",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    let last_customer_reply_at: Option<String> = conn
        .query_row(
            "SELECT MAX(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type = 'customer'",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    let last_human_agent_response_at: Option<String> = conn
        .query_row(
            "SELECT MAX(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type = 'agent'",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    // customer_waiting_since: if the last event was a customer message, set it;
    // otherwise clear it.
    let customer_waiting_since = match state {
        ResponseState::CustomerWaiting => last_customer_reply_at.clone(),
        _ => None,
    };

    conn.execute(
        "UPDATE conversations SET
            first_customer_message_at = ?1,
            first_response_at = ?2,
            last_customer_reply_at = ?3,
            last_human_agent_response_at = ?4,
            customer_waiting_since = ?5,
            response_state = ?6
         WHERE remote_id = ?7",
        params![
            first_customer_message_at,
            first_response_at,
            last_customer_reply_at,
            last_human_agent_response_at,
            customer_waiting_since,
            state.as_str(),
            conversation_remote_id,
        ],
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
        apply_m003(&conn).unwrap();
        conn
    }

    fn insert_conversation(conn: &Connection, remote_id: i64, status: &str) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (?1, ?2, ?3, 101, 2001)",
            params![remote_id, remote_id, status],
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

    #[test]
    fn m003_creates_activity_events_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m003_adds_derived_columns_to_conversations() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        // The column should exist and have a default value.
        let state: String = conn
            .query_row(
                "SELECT response_state FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "needs_first_response");
    }

    #[test]
    fn record_event_is_idempotent_on_dedup_key() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");

        let first = record_event(
            &conn,
            &ActivityEvent {
                id: None,
                conversation_id: 1001,
                event_type: "message".into(),
                actor_type: "customer".into(),
                actor_id: None,
                occurred_at: "2026-01-01T10:00:00Z".into(),
                dedup_key: "evt_001".into(),
            },
        )
        .unwrap();
        assert!(first, "first record should insert");

        let second = record_event(
            &conn,
            &ActivityEvent {
                id: None,
                conversation_id: 1001,
                event_type: "message".into(),
                actor_type: "customer".into(),
                actor_id: None,
                occurred_at: "2026-01-01T10:00:00Z".into(),
                dedup_key: "evt_001".into(),
            },
        )
        .unwrap();
        assert!(
            !second,
            "second record with same dedup_key should be skipped"
        );

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn derive_response_state_needs_first_response_when_no_events() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        let state = derive_response_state(&conn, 1001).unwrap();
        assert_eq!(state, ResponseState::NeedsFirstResponse);
    }

    #[test]
    fn derive_response_state_customer_waiting_after_customer_message() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        insert_event(
            &conn,
            1001,
            "message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_001",
        );
        let state = derive_response_state(&conn, 1001).unwrap();
        assert_eq!(state, ResponseState::CustomerWaiting);
    }

    #[test]
    fn derive_response_state_agent_waiting_after_agent_reply() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        insert_event(
            &conn,
            1001,
            "message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_001",
        );
        insert_event(
            &conn,
            1001,
            "reply",
            "agent",
            "2026-01-01T10:05:00Z",
            "evt_002",
        );
        let state = derive_response_state(&conn, 1001).unwrap();
        assert_eq!(state, ResponseState::AgentWaiting);
    }

    #[test]
    fn derive_response_state_closed_when_status_closed() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "closed");
        let state = derive_response_state(&conn, 1001).unwrap();
        assert_eq!(state, ResponseState::Closed);
    }

    #[test]
    fn update_derived_columns_sets_all_timestamps() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        insert_event(
            &conn,
            1001,
            "message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_001",
        );
        insert_event(
            &conn,
            1001,
            "reply",
            "agent",
            "2026-01-01T10:05:00Z",
            "evt_002",
        );
        insert_event(
            &conn,
            1001,
            "message",
            "customer",
            "2026-01-01T10:10:00Z",
            "evt_003",
        );

        update_derived_columns(&conn, 1001).unwrap();

        let (first_msg, first_reply, last_customer, last_agent, state): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = conn
            .query_row(
                "SELECT first_customer_message_at, first_response_at, last_customer_reply_at,
                        last_human_agent_response_at, response_state
                 FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();

        assert_eq!(first_msg, Some("2026-01-01T10:00:00Z".into()));
        assert_eq!(first_reply, Some("2026-01-01T10:05:00Z".into()));
        assert_eq!(last_customer, Some("2026-01-01T10:10:00Z".into()));
        assert_eq!(last_agent, Some("2026-01-01T10:05:00Z".into()));
        assert_eq!(state, "customer_waiting");
    }

    #[test]
    fn update_derived_columns_sets_customer_waiting_since() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        insert_event(
            &conn,
            1001,
            "message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_001",
        );

        update_derived_columns(&conn, 1001).unwrap();

        let waiting_since: Option<String> = conn
            .query_row(
                "SELECT customer_waiting_since FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(waiting_since, Some("2026-01-01T10:00:00Z".into()));
    }

    #[test]
    fn update_derived_columns_clears_waiting_since_when_agent_replies() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active");
        insert_event(
            &conn,
            1001,
            "message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_001",
        );
        insert_event(
            &conn,
            1001,
            "reply",
            "agent",
            "2026-01-01T10:05:00Z",
            "evt_002",
        );

        update_derived_columns(&conn, 1001).unwrap();

        let waiting_since: Option<String> = conn
            .query_row(
                "SELECT customer_waiting_since FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            waiting_since.is_none(),
            "customer_waiting_since should be NULL when agent replied last"
        );
    }

    #[test]
    fn m003_is_idempotent() {
        let conn = fresh_db();
        // Running M003 again should not error (columns already exist).
        apply_m003(&conn).unwrap();
    }
}
