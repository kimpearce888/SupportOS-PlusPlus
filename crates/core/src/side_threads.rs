//! Side threads — M006 migration + CRUD + list (M4-T09).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! Side threads are agent-only discussion threads attached to a conversation.
//! They are separate from the customer-visible conversation thread (per spec:
//! "side threads") — customers never see side-thread messages.
//!
//! ## Schema
//!
//! - `side_threads` — one row per thread (linked to a conversation).
//! - `side_thread_messages` — the individual messages within a thread.
//!   Each message has an optional `mentions_json` column (a JSON array of
//!   mention strings parsed from the body) so mentions can be reprocessed
//!   later (e.g. for notification replay) without re-scanning the body.
//!
//! Per KNOWN PITFALLS: all timestamp comparisons use `julianday()`.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::mentions;

/// The M006 migration: creates `side_threads` + `side_thread_messages` tables
/// + indexes.
pub const M006_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS side_threads (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id     INTEGER NOT NULL,
        created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        created_by_user_id  INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_side_threads_conversation
        ON side_threads (conversation_id, created_at);

    CREATE TABLE IF NOT EXISTS side_thread_messages (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id           INTEGER NOT NULL REFERENCES side_threads (id) ON DELETE CASCADE,
        body                TEXT NOT NULL,
        author_user_id      INTEGER,
        created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        mentions_json       TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_side_thread_messages_thread
        ON side_thread_messages (thread_id, created_at);

    UPDATE app_state SET schema_version = 6 WHERE id = 1;
"#;

/// Apply M006 migration. Idempotent.
pub fn apply_m006(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS side_threads (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id     INTEGER NOT NULL,
            created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            created_by_user_id  INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_side_threads_conversation
            ON side_threads (conversation_id, created_at);

        CREATE TABLE IF NOT EXISTS side_thread_messages (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id           INTEGER NOT NULL REFERENCES side_threads (id) ON DELETE CASCADE,
            body                TEXT NOT NULL,
            author_user_id      INTEGER,
            created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            mentions_json       TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_side_thread_messages_thread
            ON side_thread_messages (thread_id, created_at);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 6 WHERE id = 1", []);
    Ok(())
}

/// A side thread row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SideThread {
    /// The row id (assigned by SQLite on insert).
    pub id: Option<i64>,
    /// The conversation this thread is attached to (Help Scout conversation remote_id).
    pub conversation_id: i64,
    /// The agent who created the thread (Help Scout user remote_id).
    pub created_by_user_id: Option<i64>,
    /// When the thread was created (ISO-8601 UTC).
    pub created_at: String,
}

/// A side thread message row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SideThreadMessage {
    /// The row id (assigned by SQLite on insert).
    pub id: Option<i64>,
    /// The thread this message belongs to.
    pub thread_id: i64,
    /// The message body (agent-visible only; never shown to customers).
    pub body: String,
    /// The agent who wrote the message (Help Scout user remote_id).
    pub author_user_id: Option<i64>,
    /// When the message was created (ISO-8601 UTC).
    pub created_at: String,
    /// JSON-encoded array of mention strings parsed from the body.
    /// `None` if no mentions were found.
    pub mentions_json: Option<String>,
}

/// Create a new side thread attached to a conversation. Returns the new
/// thread's row id.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the insert fails.
pub fn create_side_thread(
    conn: &Connection,
    conversation_id: i64,
    created_by_user_id: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO side_threads (conversation_id, created_by_user_id)
         VALUES (?1, ?2)",
        params![conversation_id, created_by_user_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Add a message to a side thread. The body is scanned for mentions (per
/// M4-T08); the parsed mentions are stored as JSON in `mentions_json` so
/// they can be reprocessed without re-scanning.
///
/// Per spec: side thread messages are agent-only — they are NEVER shown to
/// the customer. The mention scan here records the mentions in the row
/// only; the caller is responsible for emitting notifications via
/// [`mentions::emit_mention_notifications`] if desired (separation of concerns:
/// storage vs. notification).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the insert fails, or `Error::Other` if the
/// body exceeds `mentions::MAX_BODY_BYTES`.
pub fn add_side_thread_message(
    conn: &Connection,
    thread_id: i64,
    body: &str,
    author_user_id: Option<i64>,
) -> Result<i64> {
    // Scan for mentions up front so we can store them in the row.
    // Fail-fast if the body is too large.
    let mentions = mentions::scan_for_mentions(body)?;
    let mentions_json = if mentions.is_empty() {
        None
    } else {
        let display_strings: Vec<String> =
            mentions.iter().map(mentions::Mention::display).collect();
        Some(serde_json::to_string(&display_strings).map_err(|e| {
            crate::error::Error::Config(format!("mentions_json serialization failed: {e}"))
        })?)
    };

    conn.execute(
        "INSERT INTO side_thread_messages (thread_id, body, author_user_id, mentions_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![thread_id, body, author_user_id, mentions_json],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List all side threads attached to a conversation, ordered oldest-first
/// (via `julianday(created_at)` per KNOWN PITFALLS).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_side_threads_for_conversation(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Vec<SideThread>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, created_by_user_id, created_at
         FROM side_threads
         WHERE conversation_id = ?1
         ORDER BY julianday(created_at) ASC",
    )?;
    let rows = stmt
        .query_map(params![conversation_id], |r| {
            Ok(SideThread {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                created_by_user_id: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// List all messages in a side thread, ordered oldest-first (via
/// `julianday(created_at)` per KNOWN PITFALLS).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_side_thread_messages(
    conn: &Connection,
    thread_id: i64,
) -> Result<Vec<SideThreadMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, thread_id, body, author_user_id, created_at, mentions_json
         FROM side_thread_messages
         WHERE thread_id = ?1
         ORDER BY julianday(created_at) ASC",
    )?;
    let rows = stmt
        .query_map(params![thread_id], |r| {
            Ok(SideThreadMessage {
                id: r.get(0)?,
                thread_id: r.get(1)?,
                body: r.get(2)?,
                author_user_id: r.get(3)?,
                created_at: r.get(4)?,
                mentions_json: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count the number of messages in a side thread. Used by the inbox page
/// badge (e.g. "3 messages in this side thread").
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn count_side_thread_messages(conn: &Connection, thread_id: i64) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM side_thread_messages WHERE thread_id = ?1",
        params![thread_id],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::apply_m003;
    use crate::notifications::apply_m005;
    use crate::ticket_states::apply_m004;
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
        apply_m004(&conn).unwrap();
        apply_m005(&conn).unwrap();
        apply_m006(&conn).unwrap();
        // mentions.rs depends on users + teams tables for emit_mention_notifications,
        // but the side_threads module only calls scan_for_mentions (no DB lookup).
        conn
    }

    // ---- M006 migration -----------------------------------------------------

    #[test]
    fn m006_creates_side_threads_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM side_threads", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m006_creates_side_thread_messages_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM side_thread_messages", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m006_creates_indexes() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN ('idx_side_threads_conversation',
                                'idx_side_thread_messages_thread')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn m006_is_idempotent() {
        let conn = fresh_db();
        // Re-applying M006 should not error (tables/indexes already exist).
        apply_m006(&conn).unwrap();
    }

    // ---- create_side_thread --------------------------------------------------

    #[test]
    fn create_side_thread_returns_row_id() {
        let conn = fresh_db();
        let id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn create_side_thread_stores_conversation_and_author() {
        let conn = fresh_db();
        let id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let (conv, author): (i64, Option<i64>) = conn
            .query_row(
                "SELECT conversation_id, created_by_user_id FROM side_threads WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(conv, 1001);
        assert_eq!(author, Some(42));
    }

    #[test]
    fn create_side_thread_with_no_author() {
        let conn = fresh_db();
        let id = create_side_thread(&conn, 1001, None).unwrap();
        let author: Option<i64> = conn
            .query_row(
                "SELECT created_by_user_id FROM side_threads WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(author.is_none());
    }

    // ---- add_side_thread_message --------------------------------------------

    #[test]
    fn add_side_thread_message_returns_row_id() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let msg_id = add_side_thread_message(&conn, thread_id, "Hello team", Some(42)).unwrap();
        assert!(msg_id > 0);
    }

    #[test]
    fn add_side_thread_message_stores_body_and_author() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let msg_id =
            add_side_thread_message(&conn, thread_id, "Heads up @alice", Some(42)).unwrap();
        let (body, author): (String, Option<i64>) = conn
            .query_row(
                "SELECT body, author_user_id FROM side_thread_messages WHERE id = ?1",
                params![msg_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(body, "Heads up @alice");
        assert_eq!(author, Some(42));
    }

    #[test]
    fn add_side_thread_message_stores_mentions_json_when_mentions_present() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let msg_id = add_side_thread_message(
            &conn,
            thread_id,
            "Hey @alice please ask @team:engineering",
            Some(42),
        )
        .unwrap();
        let mentions_json: Option<String> = conn
            .query_row(
                "SELECT mentions_json FROM side_thread_messages WHERE id = ?1",
                params![msg_id],
                |r| r.get(0),
            )
            .unwrap();
        let mentions_json = mentions_json.expect("mentions should be stored");
        let parsed: Vec<String> = serde_json::from_str(&mentions_json).unwrap();
        assert!(parsed.contains(&"@alice".to_string()), "parsed: {parsed:?}");
        assert!(
            parsed.contains(&"@team:engineering".to_string()),
            "parsed: {parsed:?}"
        );
    }

    #[test]
    fn add_side_thread_message_stores_null_mentions_json_when_no_mentions() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let msg_id =
            add_side_thread_message(&conn, thread_id, "No mentions here", Some(42)).unwrap();
        let mentions_json: Option<String> = conn
            .query_row(
                "SELECT mentions_json FROM side_thread_messages WHERE id = ?1",
                params![msg_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(mentions_json.is_none(), "no mentions → NULL column");
    }

    #[test]
    fn add_side_thread_message_rejects_oversized_body() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let big = "a".repeat(mentions::MAX_BODY_BYTES + 1);
        let result = add_side_thread_message(&conn, thread_id, &big, Some(42));
        assert!(result.is_err(), "oversized body must be rejected");
    }

    // ---- list_side_threads_for_conversation ---------------------------------

    #[test]
    fn list_side_threads_returns_only_threads_for_conversation() {
        let conn = fresh_db();
        let _t1 = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let _t2 = create_side_thread(&conn, 1001, Some(43)).unwrap();
        let _t3 = create_side_thread(&conn, 1002, Some(42)).unwrap();

        let threads = list_side_threads_for_conversation(&conn, 1001).unwrap();
        assert_eq!(threads.len(), 2, "only threads for conv 1001");
        for t in &threads {
            assert_eq!(t.conversation_id, 1001);
        }
    }

    #[test]
    fn list_side_threads_for_conversation_with_no_threads_returns_empty() {
        let conn = fresh_db();
        let threads = list_side_threads_for_conversation(&conn, 9999).unwrap();
        assert!(threads.is_empty());
    }

    #[test]
    fn list_side_threads_is_ordered_oldest_first_via_julianday() {
        let conn = fresh_db();
        // Insert threads with explicit created_at timestamps out of order.
        let t1 = create_side_thread(&conn, 1001, Some(42)).unwrap();
        // Set created_at to a specific past timestamp.
        conn.execute(
            "UPDATE side_threads SET created_at = '2026-01-01T10:00:00Z' WHERE id = ?1",
            params![t1],
        )
        .unwrap();
        let t2 = create_side_thread(&conn, 1001, Some(43)).unwrap();
        conn.execute(
            "UPDATE side_threads SET created_at = '2026-01-01T09:00:00Z' WHERE id = ?1",
            params![t2],
        )
        .unwrap();

        let threads = list_side_threads_for_conversation(&conn, 1001).unwrap();
        // t2 (09:00) should come before t1 (10:00).
        assert_eq!(threads[0].id, Some(t2));
        assert_eq!(threads[1].id, Some(t1));
    }

    // ---- list_side_thread_messages ------------------------------------------

    #[test]
    fn list_side_thread_messages_returns_only_messages_for_thread() {
        let conn = fresh_db();
        let t1 = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let t2 = create_side_thread(&conn, 1001, Some(43)).unwrap();
        add_side_thread_message(&conn, t1, "msg 1 in t1", Some(42)).unwrap();
        add_side_thread_message(&conn, t1, "msg 2 in t1", Some(42)).unwrap();
        add_side_thread_message(&conn, t2, "msg in t2", Some(43)).unwrap();

        let messages = list_side_thread_messages(&conn, t1).unwrap();
        assert_eq!(messages.len(), 2, "only messages in t1");
        for m in &messages {
            assert_eq!(m.thread_id, t1);
        }
    }

    #[test]
    fn list_side_thread_messages_for_empty_thread_returns_empty() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let messages = list_side_thread_messages(&conn, thread_id).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn list_side_thread_messages_is_ordered_oldest_first_via_julianday() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let m1 = add_side_thread_message(&conn, thread_id, "first", Some(42)).unwrap();
        // Set explicit timestamps out of order.
        conn.execute(
            "UPDATE side_thread_messages SET created_at = '2026-01-01T10:00:00Z' WHERE id = ?1",
            params![m1],
        )
        .unwrap();
        let m2 = add_side_thread_message(&conn, thread_id, "second", Some(42)).unwrap();
        conn.execute(
            "UPDATE side_thread_messages SET created_at = '2026-01-01T09:00:00Z' WHERE id = ?1",
            params![m2],
        )
        .unwrap();

        let messages = list_side_thread_messages(&conn, thread_id).unwrap();
        // m2 (09:00) before m1 (10:00).
        assert_eq!(messages[0].id, Some(m2));
        assert_eq!(messages[1].id, Some(m1));
    }

    // ---- count_side_thread_messages -----------------------------------------

    #[test]
    fn count_side_thread_messages_returns_zero_for_empty_thread() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let count = count_side_thread_messages(&conn, thread_id).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn count_side_thread_messages_counts_correctly() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        add_side_thread_message(&conn, thread_id, "msg 1", Some(42)).unwrap();
        add_side_thread_message(&conn, thread_id, "msg 2", Some(42)).unwrap();
        add_side_thread_message(&conn, thread_id, "msg 3", Some(42)).unwrap();
        let count = count_side_thread_messages(&conn, thread_id).unwrap();
        assert_eq!(count, 3);
    }

    // ---- SideThread + SideThreadMessage structs ------------------------------

    #[test]
    fn side_thread_serializes() {
        let t = SideThread {
            id: Some(1),
            conversation_id: 1001,
            created_by_user_id: Some(42),
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"conversation_id\":1001"));
        assert!(s.contains("\"created_by_user_id\":42"));
    }

    #[test]
    fn side_thread_message_serializes_with_mentions_json() {
        let m = SideThreadMessage {
            id: Some(1),
            thread_id: 7,
            body: "Heads up @alice".into(),
            author_user_id: Some(42),
            created_at: "2026-10-01T10:00:00Z".into(),
            mentions_json: Some(r#"["@alice"]"#.into()),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"thread_id\":7"));
        assert!(s.contains("\"body\":\"Heads up @alice\""));
        assert!(s.contains(r#""mentions_json":"[\"@alice\"]""#));
    }
}
