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

/// The full event shape — the reference `RecordEventInput` (conversationRepo
/// and activityRepo): the base [`ActivityEvent`] plus the breadth columns
/// that the sync's event derivation stamps.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FullActivityEvent {
    #[serde(flatten)]
    pub base: ActivityEvent,
    /// The local thread row the event was derived from (thread events).
    pub thread_local_id: Option<i64>,
    /// 'sync' | 'webhook' | 'local' | 'rebuild' (reference EventSource).
    pub source: String,
    /// JSON object with the diff/derivation facts.
    pub metadata: Option<String>,
}

/// Record an event with the full reference shape. Idempotent on the dedup
/// key; stamps the breadth columns when the schema has them (older chains
/// fall back to the base columns). Returns `true` when a NEW row landed.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the insert fails.
pub fn record_full_event(conn: &Connection, event: &FullActivityEvent) -> Result<bool> {
    let has_columns = conn
        .prepare("SELECT metadata, source, thread_local_id FROM activity_events LIMIT 0")
        .is_ok();
    if !has_columns {
        return record_event(conn, &event.base);
    }
    let rows = conn.execute(
        "INSERT OR IGNORE INTO activity_events
            (conversation_id, event_type, actor_type, actor_id, occurred_at, dedup_key,
             thread_local_id, source, metadata)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event.base.conversation_id,
            event.base.event_type,
            event.base.actor_type,
            event.base.actor_id,
            event.base.occurred_at,
            event.base.dedup_key,
            event.thread_local_id,
            event.source,
            event.metadata,
        ],
    )?;
    Ok(rows > 0)
}

/// The port's `nowIso()` — observation stamps for sync-derived events.
#[must_use]
pub fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
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

/// Counts returned by [`rebuild_all`] — the rebuildAll admin-action summary
/// (audit AC-03: `POST /api/conversations/activity/rebuild` used to answer
/// `ok:true` without touching any data).
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct RebuildSummary {
    /// Conversations scanned (every row in `conversations`).
    pub conversations: i64,
    /// NEW activity events derived from the thread mirror — re-derivations
    /// of events the sync path already wrote (deduped on `thread:{remote_id}`)
    /// count as zero.
    pub events_written: i64,
    /// Total activity events in the table after the rebuild.
    pub events_total: i64,
}

/// One row of the thread mirror as the rebuild reads it (the DB-04/M045
/// reference actor model: `from_type` + the three-way `created_by_*`
/// split).
struct MirrorThread {
    id: i64,
    remote_id: Option<i64>,
    thread_type: String,
    state: Option<String>,
    body: Option<String>,
    from_type: Option<String>,
    created_by_user_id: Option<i64>,
    created_by_customer_id: Option<i64>,
    created_at: Option<String>,
}

/// Derive the event triple for one mirror thread — the same mapping the
/// sync path's `record_thread_event` applies (sync_engine.rs), read from
/// the stored mirror instead of a provider payload:
/// `note` → `internal_note` (user actor); `lineitem` → the conservative
/// status/assign/moved/tag text mapping with `system_user`; everything else
/// splits on the stored 3-way actor (`created_by_customer_id` → customer
/// message, `created_by_user_id` → human agent message, unknown fallback).
fn derive_mirror_event(t: &MirrorThread) -> Option<(String, String, Option<i64>)> {
    // Drafts / scheduled replies are not history yet — the sync path only
    // derives events for `state = 'published'` (a NULL state on an older
    // row means "was never a draft").
    if t.state.as_deref().is_some_and(|s| s != "published") {
        return None;
    }
    match t.thread_type.as_str() {
        "note" => Some(("internal_note".into(), "user".into(), t.created_by_user_id)),
        "lineitem" => {
            let text = t.body.as_deref().unwrap_or("").to_lowercase();
            let event_type = if text.contains("status") {
                "status_changed"
            } else if text.contains("assign") {
                "assignment_changed"
            } else if text.contains("moved") {
                "moved"
            } else if text.contains("tag") {
                "tag_added"
            } else {
                "lineitem_action"
            };
            Some((event_type.into(), "system_user".into(), None))
        }
        _ => match t.from_type.as_deref().unwrap_or("") {
            "customer" => Some((
                "customer_message".into(),
                "customer".into(),
                t.created_by_customer_id,
            )),
            "user" | "agent" => Some((
                "human_agent_message".into(),
                "user".into(),
                t.created_by_user_id,
            )),
            _ => Some(("customer_message".into(), "unknown".into(), None)),
        },
    }
}

/// Recompute the derived columns for one conversation keyed by its LOCAL id
/// — the join the rebuild uses, because sync-written events (and the
/// rebuild's own) carry the local `conversations.id`, the same key the
/// timeline reads use (audit AC-04). Unlike [`update_derived_columns`]
/// (keyed by remote id for the local-mutation paths), this reads the full
/// event set. The agent-side actor is matched on both the sync vocabulary
/// (`user`) and the legacy local one (`agent`).
fn update_derived_columns_by_local_id(conn: &Connection, conv_local: i64) -> Result<()> {
    let status: Option<String> = conn
        .query_row(
            "SELECT status FROM conversations WHERE id = ?1",
            params![conv_local],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    // Response state: closed wins; otherwise the LAST event's actor side
    // decides (customer → waiting on us, agent/user → waiting on them;
    // system-ish events leave the state unchanged at the default).
    let state = if status.as_deref() == Some("closed") {
        ResponseState::Closed
    } else {
        let last_event: Option<String> = conn
            .query_row(
                "SELECT actor_type FROM activity_events
                 WHERE conversation_id = ?1
                 ORDER BY julianday(occurred_at) DESC, id DESC
                 LIMIT 1",
                params![conv_local],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        match last_event.as_deref() {
            Some("customer") => ResponseState::CustomerWaiting,
            Some("user" | "agent") => ResponseState::AgentWaiting,
            _ => ResponseState::NeedsFirstResponse,
        }
    };

    // Derived timestamps from the (now complete) event set.
    let first_customer_message_at: Option<String> = conn
        .query_row(
            "SELECT MIN(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type = 'customer'",
            params![conv_local],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let first_response_at: Option<String> = conn
        .query_row(
            "SELECT MIN(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type IN ('agent','user')
               AND event_type IN ('reply','human_agent_message')",
            params![conv_local],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let last_customer_reply_at: Option<String> = conn
        .query_row(
            "SELECT MAX(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type = 'customer'",
            params![conv_local],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let last_human_agent_response_at: Option<String> = conn
        .query_row(
            "SELECT MAX(occurred_at) FROM activity_events
             WHERE conversation_id = ?1 AND actor_type IN ('agent','user')",
            params![conv_local],
            |r| r.get(0),
        )
        .ok()
        .flatten();
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
         WHERE id = ?7",
        params![
            first_customer_message_at,
            first_response_at,
            last_customer_reply_at,
            last_human_agent_response_at,
            customer_waiting_since,
            state.as_str(),
            conv_local,
        ],
    )?;
    Ok(())
}

/// The rebuildAll admin action (audit AC-03): re-derive every conversation's
/// activity events from the thread mirror, then recompute the derived
/// columns (timestamps + response state) from the event table.
///
/// Idempotent by design — "Dedup keys on every derived event so rebuilds
/// and re-syncs are idempotent": mirrored threads derive `thread:{remote_id}`
/// (the exact keys the sync path writes, so a rebuild after a healthy sync
/// adds nothing), and local-only threads without a remote id derive the
/// stable `rebuild:thread:{local_id}`. Rebuilt events carry
/// `source='rebuild'` (the reference EventSource vocabulary) and the
/// conversation's LOCAL id (the canonical timeline join).
///
/// This is the function behind the `rebuild_activity` maintenance job the
/// `POST /api/conversations/activity/rebuild` route enqueues; it never
/// touches the provider — the mirror is the source of truth.
pub fn rebuild_all(conn: &Connection) -> Result<RebuildSummary> {
    let mut summary = RebuildSummary::default();

    let conversations: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM conversations")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.filter_map(|r| r.ok()).collect()
    };
    summary.conversations = conversations.len() as i64;

    for conv_local in conversations {
        // 1. Re-derive events from the thread mirror (published, not
        //    soft-deleted threads only).
        let threads: Vec<MirrorThread> = {
            let mut stmt = conn.prepare(
                "SELECT id, remote_id, type, state, body_text, from_type,
                        created_by_user_id, created_by_customer_id, created_at
                 FROM conversation_threads
                 WHERE conversation_id = ?1 AND deleted_at IS NULL",
            )?;
            let rows = stmt.query_map(params![conv_local], |r| {
                Ok(MirrorThread {
                    id: r.get(0)?,
                    remote_id: r.get(1)?,
                    thread_type: r.get(2)?,
                    state: r.get(3)?,
                    body: r.get(4)?,
                    from_type: r.get(5)?,
                    created_by_user_id: r.get(6)?,
                    created_by_customer_id: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?;
            rows.filter_map(|r| r.ok()).collect()
        };

        for t in &threads {
            let Some((event_type, actor_type, actor_id)) = derive_mirror_event(t) else {
                continue;
            };
            let dedup_key = match t.remote_id {
                Some(remote) => format!("thread:{remote}"),
                None => format!("rebuild:thread:{}", t.id),
            };
            // A missing timestamp only happens on hand-inserted rows; the
            // observation time is the honest fallback (an empty string would
            // poison the julianday ordering).
            let occurred_at = t
                .created_at
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(now_iso);
            let wrote = record_full_event(
                conn,
                &FullActivityEvent {
                    base: ActivityEvent {
                        id: None,
                        conversation_id: conv_local,
                        event_type,
                        actor_type,
                        actor_id,
                        occurred_at,
                        dedup_key,
                    },
                    thread_local_id: Some(t.id),
                    source: "rebuild".into(),
                    metadata: Some(
                        serde_json::json!({
                            "thread_remote_id": t.remote_id,
                            "thread_type": t.thread_type,
                        })
                        .to_string(),
                    ),
                },
            )?;
            if wrote {
                summary.events_written += 1;
            }
        }

        // 2. Recompute the derived columns from the (now complete) event set.
        update_derived_columns_by_local_id(conn, conv_local)?;
    }

    summary.events_total = conn
        .query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
        .unwrap_or(0);
    Ok(summary)
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
        // DB-03 (M047): the reference conversations column names — a
        // test-local RENAME of the three port columns keeps this slim chain
        // (run_all + m003) intact while the fixtures below insert the
        // post-migration shape. Like the thread mirror below, the slim
        // fixture carries no FKs; the real chain's M047 rebuild (with FKs +
        // UNIQUE(number)) only runs on full boots.
        conn.execute_batch(
            "ALTER TABLE conversations RENAME COLUMN mailbox_id TO mailbox_local_id;
             ALTER TABLE conversations RENAME COLUMN assignee_id TO assignee_local_id;
             ALTER TABLE conversations RENAME COLUMN customer_id TO customer_local_id;",
        )
        .unwrap();
        conn
    }

    fn insert_conversation(conn: &Connection, remote_id: i64, status: &str) {
        // Parent rows for the concrete ids (the slim chain declares no FKs,
        // but the real M047 shape enforces them — keep the fixture honest).
        conn.execute(
            "INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (101, 101, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (2001, 2001, 'Cust')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_local_id, customer_local_id)
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

    // ---- AC-03: the rebuildAll admin action --------------------------------

    /// The minimal thread-mirror shape the rebuild reads (the full chain's
    /// table comes from later migrations; the unit tests here run a slim
    /// M003-only database). DB-04 (M045): the reference actor model —
    /// `type` / `body_text` / `from_type` + the three-way `created_by_*`
    /// split (no FKs on the slim fixture; the real chain enforces them).
    fn ensure_thread_mirror(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversation_threads (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER NOT NULL,
                remote_id       INTEGER,
                type            TEXT NOT NULL,
                state           TEXT DEFAULT 'published',
                body_text       TEXT,
                from_type       TEXT,
                created_by_user_id        INTEGER,
                created_by_customer_id    INTEGER,
                created_by_system_user_id INTEGER,
                created_at      TEXT,
                deleted_at      TEXT
            );",
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)] // a test fixture row — the mirror's shape
    fn insert_thread(
        conn: &Connection,
        conv_local: i64,
        remote_id: Option<i64>,
        thread_type: &str,
        state: Option<&str>,
        body: &str,
        actor_type: &str,
        actor_id: Option<i64>,
        created_at: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO conversation_threads
                (conversation_id, remote_id, type, state, body_text, from_type,
                 created_by_user_id, created_by_customer_id,
                 created_by_system_user_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5,
                     CASE WHEN ?6 = 'system' THEN 'system_user' ELSE ?6 END,
                     CASE WHEN ?6 = 'user' THEN ?7 END,
                     CASE WHEN ?6 = 'customer' THEN ?7 END,
                     CASE WHEN ?6 IN ('system', 'system_user') THEN ?7 END,
                     ?8)",
            params![
                conv_local,
                remote_id,
                thread_type,
                state,
                body,
                actor_type,
                actor_id,
                created_at
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    // The shared rebuild-fixture: a fresh DB with the thread mirror + one
    // conversation whose local id (9) and remote id (5001) differ.
    fn rebuild_db() -> Connection {
        let conn = fresh_db();
        ensure_thread_mirror(&conn);
        // Parent rows for the concrete ids (mailbox 1 / customer 1) — see
        // the fresh_db note; the real M047 shape enforces the FKs.
        conn.execute(
            "INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (1, 1, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (1, 1, 'Cust')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (id, remote_id, number, status, mailbox_local_id, customer_local_id)
             VALUES (9, 5001, 101, 'active', 1, 1)",
            [],
        )
        .unwrap();
        conn
    }

    fn event_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM activity_events", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn rebuild_all_derives_events_from_the_thread_mirror() {
        let conn = rebuild_db();
        insert_thread(
            &conn,
            9,
            Some(7001),
            "customer",
            Some("published"),
            "I want a refund",
            "customer",
            Some(1),
            "2026-01-01T10:00:00Z",
        );
        insert_thread(
            &conn,
            9,
            Some(7002),
            "reply",
            Some("published"),
            "We are on it",
            "user",
            Some(2),
            "2026-01-01T10:05:00Z",
        );
        insert_thread(
            &conn,
            9,
            Some(7003),
            "note",
            Some("published"),
            "VIP customer",
            "user",
            Some(2),
            "2026-01-01T10:06:00Z",
        );
        // A draft reply and a scheduled one must NOT become history.
        insert_thread(
            &conn,
            9,
            Some(7004),
            "reply",
            Some("draft"),
            "unsent draft",
            "user",
            Some(2),
            "2026-01-01T11:00:00Z",
        );
        insert_thread(
            &conn,
            9,
            Some(7005),
            "reply",
            Some("scheduled"),
            "scheduled reply",
            "user",
            Some(2),
            "2026-01-01T12:00:00Z",
        );
        // A lineitem maps through the conservative text classification
        // (deliberately before the note: lineitems are system events).
        insert_thread(
            &conn,
            9,
            Some(7006),
            "lineitem",
            Some("published"),
            "changed status from active to pending",
            "system_user",
            None,
            "2026-01-01T10:04:00Z",
        );

        let summary = rebuild_all(&conn).unwrap();

        assert_eq!(summary.conversations, 1);
        assert_eq!(
            summary.events_written, 4,
            "customer+reply+note+lineitem (draft/scheduled skipped)"
        );
        assert_eq!(summary.events_total, 4);

        // The events carry the LOCAL conversation id (the canonical join the
        // timeline reads use) and the sync-path dedup keys. (source lives on
        // the full-bootstrap schema; record_full_event falls back to the
        // base columns on the slim M003-only chain — the integration test
        // covers the source='rebuild' stamp end-to-end.)
        let rows: Vec<(i64, String, String, Option<i64>, String)> = conn
            .prepare(
                "SELECT conversation_id, event_type, actor_type, actor_id, dedup_key
                 FROM activity_events ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[0],
            (
                9,
                "customer_message".into(),
                "customer".into(),
                Some(1),
                "thread:7001".into()
            )
        );
        assert_eq!(
            rows[1],
            (
                9,
                "human_agent_message".into(),
                "user".into(),
                Some(2),
                "thread:7002".into()
            )
        );
        assert_eq!(
            rows[2],
            (
                9,
                "internal_note".into(),
                "user".into(),
                Some(2),
                "thread:7003".into()
            )
        );
        assert_eq!(
            rows[3],
            (
                9,
                "status_changed".into(),
                "system_user".into(),
                None,
                "thread:7006".into()
            )
        );

        // Derived columns recomputed from the LOCAL-id event join: the note
        // (user, 10:06) is the last actor-bearing event → agent_waiting.
        let (first_msg, first_reply, state): (Option<String>, Option<String>, String) = conn
            .query_row(
                "SELECT first_customer_message_at, first_response_at, response_state
                 FROM conversations WHERE id = 9",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(first_msg.as_deref(), Some("2026-01-01T10:00:00Z"));
        assert_eq!(first_reply.as_deref(), Some("2026-01-01T10:05:00Z"));
        assert_eq!(state, "agent_waiting");
    }

    #[test]
    fn rebuild_all_dedupes_against_sync_written_events() {
        let conn = rebuild_db();
        insert_thread(
            &conn,
            9,
            Some(7001),
            "customer",
            Some("published"),
            "I want a refund",
            "customer",
            Some(1),
            "2026-01-01T10:00:00Z",
        );
        // Exactly what the sync path would have written for that thread.
        record_full_event(
            &conn,
            &FullActivityEvent {
                base: ActivityEvent {
                    id: None,
                    conversation_id: 9,
                    event_type: "customer_message".into(),
                    actor_type: "customer".into(),
                    actor_id: Some(1),
                    occurred_at: "2026-01-01T10:00:00Z".into(),
                    dedup_key: "thread:7001".into(),
                },
                thread_local_id: None,
                source: "sync".into(),
                metadata: None,
            },
        )
        .unwrap();

        let summary = rebuild_all(&conn).unwrap();

        assert_eq!(
            summary.events_written, 0,
            "the sync-written event must not duplicate"
        );
        assert_eq!(summary.events_total, 1);
        // The derived columns still got recomputed from the existing event.
        let (state, waiting_since): (String, Option<String>) = conn
            .query_row(
                "SELECT response_state, customer_waiting_since FROM conversations WHERE id = 9",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "customer_waiting");
        assert_eq!(waiting_since.as_deref(), Some("2026-01-01T10:00:00Z"));
    }

    #[test]
    fn rebuild_all_is_idempotent_across_runs() {
        let conn = rebuild_db();
        // A local-only thread (no remote id) still derives an event — with
        // the stable rebuild-scoped dedup key.
        insert_thread(
            &conn,
            9,
            None,
            "customer",
            Some("published"),
            "walk-in message",
            "customer",
            Some(1),
            "2026-01-01T10:00:00Z",
        );

        let first = rebuild_all(&conn).unwrap();
        assert_eq!(first.events_written, 1);
        assert_eq!(first.events_total, 1);
        let dedup: String = conn
            .query_row("SELECT dedup_key FROM activity_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(dedup, "rebuild:thread:1");

        let second = rebuild_all(&conn).unwrap();
        assert_eq!(second.events_written, 0, "second run writes nothing");
        assert_eq!(second.events_total, 1, "no duplicates");
    }

    #[test]
    fn rebuild_all_marks_closed_conversations() {
        let conn = rebuild_db();
        conn.execute(
            "UPDATE conversations SET status = 'closed' WHERE id = 9",
            [],
        )
        .unwrap();
        insert_thread(
            &conn,
            9,
            Some(7001),
            "customer",
            Some("published"),
            "I want a refund",
            "customer",
            Some(1),
            "2026-01-01T10:00:00Z",
        );

        rebuild_all(&conn).unwrap();

        let state: String = conn
            .query_row(
                "SELECT response_state FROM conversations WHERE id = 9",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "closed");
        // Waiting-since only applies while actually waiting.
        let waiting: Option<String> = conn
            .query_row(
                "SELECT customer_waiting_since FROM conversations WHERE id = 9",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(waiting.is_none());
    }

    #[test]
    fn rebuild_all_skips_soft_deleted_threads() {
        let conn = rebuild_db();
        let tid = insert_thread(
            &conn,
            9,
            Some(7001),
            "customer",
            Some("published"),
            "I want a refund",
            "customer",
            Some(1),
            "2026-01-01T10:00:00Z",
        );
        conn.execute(
            "UPDATE conversation_threads SET deleted_at = '2026-01-02T00:00:00Z' WHERE id = ?1",
            params![tid],
        )
        .unwrap();

        let summary = rebuild_all(&conn).unwrap();
        assert_eq!(
            summary.events_written, 0,
            "soft-deleted threads are not history"
        );
        assert_eq!(summary.events_total, 0);
        // No events → the state stays at the default.
        let state: String = conn
            .query_row(
                "SELECT response_state FROM conversations WHERE id = 9",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "needs_first_response");
    }

    #[test]
    fn rebuild_all_recomputes_stale_derived_columns() {
        let conn = rebuild_db();
        // The event exists but the derived columns were never maintained
        // (the exact stale-mirror state the rebuild exists to repair).
        insert_event(
            &conn,
            9,
            "customer_message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_x",
        );

        let before: String = conn
            .query_row(
                "SELECT response_state FROM conversations WHERE id = 9",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, "needs_first_response", "stale pre-state");

        rebuild_all(&conn).unwrap();

        let (state, waiting_since, first_msg): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT response_state, customer_waiting_since, first_customer_message_at
                 FROM conversations WHERE id = 9",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, "customer_waiting");
        assert_eq!(waiting_since.as_deref(), Some("2026-01-01T10:00:00Z"));
        assert_eq!(first_msg.as_deref(), Some("2026-01-01T10:00:00Z"));
    }

    #[test]
    fn rebuild_all_on_empty_db_is_a_no_op() {
        let conn = fresh_db();
        ensure_thread_mirror(&conn);
        let summary = rebuild_all(&conn).unwrap();
        assert_eq!(summary.conversations, 0);
        assert_eq!(summary.events_written, 0);
        assert_eq!(summary.events_total, 0);
        assert_eq!(event_count(&conn), 0);
    }
}
