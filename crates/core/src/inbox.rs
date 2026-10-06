//! Inbox module — list + filter + thread reads.
//!
//! Per spec M3: "inbox." Per A11: the inbox is the main ticket view.
//! Per KNOWN PITFALLS: "every view has loading, empty and error states."
//!
//! This module provides:
//! - `list_conversations` — paginated list with filters (status, mailbox,
//!   assignee, priority) + `extra_where` composition point for the
//!   Operations Center drill-down and saved-view fragments.
//! - `get_conversation` — full conversation details + thread (activity events
//!   + customer messages + replies + notes).
//!
//! Mutations live in `conversation_ops` (the write-protection pipeline,
//! operations.ts parity). Reads use parameterized SQL — no string
//! interpolation.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Apply the M028 migration: `conversation_threads` table.
/// Stores customer messages, replies, notes, and system events.
pub fn apply_m028(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversation_threads (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL,
            thread_type     TEXT NOT NULL,
            body            TEXT,
            actor_type      TEXT NOT NULL,
            actor_id        INTEGER,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_conv_threads_conv
            ON conversation_threads (conversation_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_conv_threads_type
            ON conversation_threads (thread_type);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 28 WHERE id = 1", []);
    Ok(())
}

/// The type of a thread entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadType {
    /// A message from the customer.
    CustomerMessage,
    /// A reply from an agent.
    Reply,
    /// An internal note (visible only to the team, not the customer).
    Note,
    /// A system event (status change, assignment, etc.).
    System,
}

impl ThreadType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CustomerMessage => "customer_message",
            Self::Reply => "reply",
            Self::Note => "note",
            Self::System => "system",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "customer_message" => Some(Self::CustomerMessage),
            "reply" => Some(Self::Reply),
            "note" => Some(Self::Note),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// A conversation list item — the minimal view shown in the inbox list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationListItem {
    pub id: i64,
    pub remote_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub status: String,
    pub mailbox_id: i64,
    pub mailbox_name: Option<String>,
    pub assignee_id: Option<i64>,
    pub assignee_name: Option<String>,
    pub customer_id: i64,
    pub customer_name: Option<String>,
    pub priority: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    /// Response state from M004 (e.g., "needs_first_response",
    /// "waiting_over_threshold", "sla_breached").
    pub response_state: Option<String>,
}

/// Filters for the inbox list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InboxFilters {
    /// Filter by status (active/pending/closed). None = all statuses.
    pub status: Option<String>,
    /// Filter by mailbox. None = all mailboxes.
    pub mailbox_id: Option<i64>,
    /// Filter by assignee. None = any assignee.
    pub assignee_id: Option<i64>,
    /// Filter by customer. None = any customer.
    pub customer_id: Option<i64>,
    /// Filter by priority. None = any priority.
    pub priority: Option<String>,
    /// Free-text search in subject + preview.
    pub query: Option<String>,
    /// Exact conversation-number lookup (?number=N — v2.0.0 M4: deep
    /// links and the incident link-by-number flow). None = no filter.
    pub number: Option<i64>,
    /// Limit (default 50, max 200).
    pub limit: Option<u32>,
    /// Offset for pagination.
    pub offset: Option<u32>,
    /// A composed WHERE fragment (no leading WHERE) from the saved-view
    /// engine or the Operations Center tile fragments (reference
    /// `extraWhere`/`extraParams` in conversations.ts:79-114). ANDed with the
    /// simple filters; identifiers inside are compiler-whitelisted literals.
    #[serde(skip)]
    pub extra_where: Option<String>,
    /// The bound parameters matching `extra_where`, in order.
    #[serde(skip)]
    pub extra_params: Vec<rusqlite::types::Value>,
}

/// A thread entry — one item in the conversation timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadEntry {
    pub id: i64,
    pub conversation_id: i64,
    pub thread_type: String,
    pub body: Option<String>,
    pub actor_type: String,
    pub actor_id: Option<i64>,
    pub actor_name: Option<String>,
    pub created_at: String,
}

/// A full conversation with its thread + customer info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDetail {
    pub id: i64,
    pub remote_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub preview: Option<String>,
    pub status: String,
    pub mailbox_id: i64,
    pub mailbox_name: Option<String>,
    pub assignee_id: Option<i64>,
    pub assignee_name: Option<String>,
    pub customer_id: i64,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub priority: Option<String>,
    pub response_state: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    /// The thread entries (oldest first).
    pub thread: Vec<ThreadEntry>,
    /// Tags on this conversation (from the conversations_tags join table if
    /// it exists; empty otherwise).
    pub tags: Vec<String>,
}

// ─── Reads ────────────────────────────────────────────────────────────────

/// List conversations matching the given filters. Returns the items + the
/// total count (for pagination).
pub fn list_conversations(
    conn: &Connection,
    filters: &InboxFilters,
) -> Result<(Vec<ConversationListItem>, u32)> {
    let limit = filters.limit.unwrap_or(50).min(200);
    let offset = filters.offset.unwrap_or(0);

    let mut where_parts: Vec<String> = Vec::new();
    let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();

    // DB-05 (M18): soft-delete + merge semantics — the inbox list only ever
    // serves live conversations (conversationRepo.ts listConversations:
    // `where = ["c.deleted_at IS NULL", "c.merged_into_conversation_id IS
    // NULL"]`). Soft-deleted rows stay in the mirror (history/reporting) but
    // leave every listing.
    where_parts.push("c.deleted_at IS NULL".to_string());
    where_parts.push("c.merged_into_conversation_id IS NULL".to_string());

    if let Some(ref status) = filters.status {
        where_parts.push("c.status = ?".to_string());
        params_vec.push(status.clone().into());
    }
    if let Some(mailbox_id) = filters.mailbox_id {
        where_parts.push("c.mailbox_id = ?".to_string());
        params_vec.push(mailbox_id.into());
    }
    if let Some(assignee_id) = filters.assignee_id {
        where_parts.push("c.assignee_id = ?".to_string());
        params_vec.push(assignee_id.into());
    }
    if let Some(customer_id) = filters.customer_id {
        where_parts.push("c.customer_id = ?".to_string());
        params_vec.push(customer_id.into());
    }
    if let Some(ref priority) = filters.priority {
        where_parts.push("c.priority = ?".to_string());
        params_vec.push(priority.clone().into());
    }
    // v2.0.0 (M4): exact number lookup — takes precedence over the
    // free-text query when present.
    if let Some(number) = filters.number {
        where_parts.push("c.number = ?".to_string());
        params_vec.push(number.into());
    }
    if let Some(ref query) = filters.query {
        let q = format!("%{query}%");
        where_parts
            .push("(c.subject LIKE ? ESCAPE '\\' OR c.preview LIKE ? ESCAPE '\\')".to_string());
        params_vec.push(q.clone().into());
        params_vec.push(q.into());
    }
    // The composed fragment (ops drill-down / saved view): identifiers are
    // whitelisted literals from the fragment compilers; values stay bound.
    if let Some(ref extra) = filters.extra_where {
        where_parts.push(format!("({extra})"));
        params_vec.extend(filters.extra_params.iter().cloned());
    }

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_parts.join(" AND "))
    };

    // Count query.
    let count_sql = format!("SELECT COUNT(*) FROM conversations c {where_clause}");
    let total: i64 = conn
        .query_row(&count_sql, params_from_iter_ref(&params_vec), |r| r.get(0))
        .unwrap_or(0);
    let total = u32::try_from(total).unwrap_or(0);

    // List query — join with mailboxes, users, customers for display names.
    let list_sql = format!(
        "SELECT c.id, c.remote_id, c.number, c.subject, c.preview, c.status,
                c.mailbox_id, m.name,
                c.assignee_id, u.first_name || ' ' || u.last_name,
                c.customer_id, cu.first_name || ' ' || cu.last_name,
                c.priority, c.created_at, c.updated_at, c.closed_at,
                c.response_state
         FROM conversations c
         LEFT JOIN mailboxes m ON m.id = c.mailbox_id
         LEFT JOIN users u ON u.id = c.assignee_id
         LEFT JOIN customers cu ON cu.id = c.customer_id
         {where_clause}
         ORDER BY COALESCE(c.updated_at, c.local_created_at) DESC
         LIMIT ? OFFSET ?"
    );

    let mut params_with_pagination = params_vec.clone();
    params_with_pagination.push(i64::from(limit).into());
    params_with_pagination.push(i64::from(offset).into());

    let mut stmt = conn.prepare(&list_sql)?;
    let items: Result<Vec<ConversationListItem>> = stmt
        .query_map(params_from_iter_ref(&params_with_pagination), |row| {
            Ok(ConversationListItem {
                id: row.get(0)?,
                remote_id: row.get(1)?,
                number: row.get(2)?,
                subject: row.get(3)?,
                preview: row.get(4)?,
                status: row.get(5)?,
                mailbox_id: row.get(6)?,
                mailbox_name: row.get(7)?,
                assignee_id: row.get(8)?,
                assignee_name: row.get(9)?,
                customer_id: row.get(10)?,
                customer_name: row.get(11)?,
                priority: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
                closed_at: row.get(15)?,
                response_state: row.get::<_, Option<String>>(16).ok().flatten(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.into());
    let items = items?;

    Ok((items, total))
}

/// Get a single conversation with its full thread.
pub fn get_conversation(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Option<ConversationDetail>> {
    let row = conn.query_row(
        "SELECT c.id, c.remote_id, c.number, c.subject, c.preview, c.status,
                    c.mailbox_id, m.name,
                    c.assignee_id, u.first_name || ' ' || u.last_name,
                    c.customer_id, cu.first_name || ' ' || cu.last_name, cu.email,
                    c.priority, c.response_state,
                    c.created_at, c.updated_at, c.closed_at
             FROM conversations c
             LEFT JOIN mailboxes m ON m.id = c.mailbox_id
             LEFT JOIN users u ON u.id = c.assignee_id
             LEFT JOIN customers cu ON cu.id = c.customer_id
             WHERE c.id = ?",
        params![conversation_id],
        |row| {
            Ok(ConversationDetail {
                id: row.get(0)?,
                remote_id: row.get(1)?,
                number: row.get(2)?,
                subject: row.get(3)?,
                preview: row.get(4)?,
                status: row.get(5)?,
                mailbox_id: row.get(6)?,
                mailbox_name: row.get(7)?,
                assignee_id: row.get(8)?,
                assignee_name: row.get(9)?,
                customer_id: row.get(10)?,
                customer_name: row.get(11)?,
                customer_email: row.get(12)?,
                priority: row.get(13)?,
                response_state: row.get::<_, Option<String>>(14).ok().flatten(),
                created_at: row.get(15)?,
                updated_at: row.get(16)?,
                closed_at: row.get(17)?,
                thread: Vec::new(),
                tags: Vec::new(),
            })
        },
    );
    let mut detail = match row {
        Ok(d) => d,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    // Load the thread.
    let mut stmt = conn.prepare(
        "SELECT t.id, t.conversation_id, t.thread_type, t.body,
                t.actor_type, t.actor_id,
                CASE WHEN t.actor_type = 'user' THEN u.first_name || ' ' || u.last_name
                     WHEN t.actor_type = 'customer' THEN cu.first_name || ' ' || cu.last_name
                     ELSE NULL END,
                t.created_at
         FROM conversation_threads t
         LEFT JOIN users u ON u.id = t.actor_id AND t.actor_type = 'user'
         LEFT JOIN customers cu ON cu.id = t.actor_id AND t.actor_type = 'customer'
         WHERE t.conversation_id = ?
         ORDER BY t.created_at ASC",
    )?;
    let thread: Result<Vec<ThreadEntry>> = stmt
        .query_map(params![conversation_id], |row| {
            Ok(ThreadEntry {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                thread_type: row.get(2)?,
                body: row.get(3)?,
                actor_type: row.get(4)?,
                actor_id: row.get(5)?,
                actor_name: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| e.into());
    detail.thread = thread?;

    Ok(Some(detail))
}

// ─── helpers ─────────────────────────────────────────────────────────────

/// Adapter so we can pass a `&[Value]` to rusqlite's `params_from_iter`.
/// The `use<'_>` bound is required for RPIT lifetime capture on Rust 2024.
fn params_from_iter_ref(values: &[rusqlite::types::Value]) -> impl rusqlite::Params + use<'_> {
    rusqlite::params_from_iter(values.iter().cloned())
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
        apply_m028(&conn).unwrap();
        // DB-05: the boot-invariant soft-delete + merge columns (sla owns
        // deleted_at; m036 owns merged_into_conversation_id) — the list query
        // filters on both, so the fixture must match the booted schema. The
        // guarded ALTERs are exactly what those boot ensures do.
        let _ = conn.execute("ALTER TABLE conversations ADD COLUMN deleted_at TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE conversations ADD COLUMN merged_into_conversation_id INTEGER",
            [],
        );
        conn
    }

    fn seed_test_data(conn: &mut Connection) -> i64 {
        // Insert a mailbox, user, customer, and a conversation.
        conn.execute(
            "INSERT INTO mailboxes (remote_id, name, slug) VALUES (101, 'Test Mailbox', 'test')",
            [],
        )
        .unwrap();
        let mailbox_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO users (remote_id, first_name, last_name, email) VALUES (201, 'Alice', 'Agent', 'alice@example.com')",
            [],
        )
        .unwrap();
        let user_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name, email) VALUES (301, 'Bob', 'Customer', 'bob@example.com')",
            [],
        )
        .unwrap();
        let customer_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, preview, status, mailbox_id, assignee_id, customer_id, priority)
             VALUES (1001, 1001, 'Test subject', 'Test preview', 'active', ?1, ?2, ?3, 'normal')",
            params![mailbox_id, user_id, customer_id],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    // ---- DB-05 (M18): soft-delete + merge semantics -------------------------

    #[test]
    fn list_conversations_excludes_soft_deleted_and_merged() {
        let mut conn = fresh_db();
        let live = seed_test_data(&mut conn);
        // Two more conversations for the same customer.
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id)
             VALUES (1002, 1002, 'To be deleted', 'active', 101,
                      (SELECT customer_id FROM conversations WHERE id = ?1))",
            params![live],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id)
             VALUES (1003, 1003, 'To be merged', 'active', 101,
                      (SELECT customer_id FROM conversations WHERE id = ?1))",
            params![live],
        )
        .unwrap();
        // Soft-delete one, merge-mark the other (conversationRepo.ts:386).
        conn.execute(
            "UPDATE conversations SET deleted_at = datetime('now') WHERE remote_id = 1002",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE conversations SET merged_into_conversation_id = ?1 WHERE remote_id = 1003",
            params![live],
        )
        .unwrap();
        let (items, total) =
            list_conversations(&conn, &crate::inbox::InboxFilters::default()).unwrap();
        assert_eq!(total, 1, "soft-deleted + merged rows leave the listing");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, live, "the live row is the one served");
    }

    #[test]
    fn upsert_conversation_resurrects_soft_deleted_rows() {
        use crate::helpscout::HsConversation;
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        let upsert = |remote: i64| {
            crate::sync::upsert_conversation(
                &conn,
                &HsConversation {
                    remote_id: remote,
                    number: remote,
                    subject: Some("resurrect me".into()),
                    preview: None,
                    status: "active".into(),
                    mailbox_id: 101,
                    customer_id: 301,
                    ..Default::default()
                },
            )
            .unwrap();
        };
        upsert(1001);
        conn.execute(
            "UPDATE conversations SET deleted_at = datetime('now') WHERE remote_id = 1001",
            [],
        )
        .unwrap();
        let hidden: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE remote_id = 1001 AND deleted_at IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hidden, 1, "soft-deleted first");
        // The row reappears remotely (undelete / restore) — the next sync's
        // upsert must resurrect it (conversationRepo.ts:136 `deleted_at=NULL`).
        upsert(1001);
        let resurrected: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE remote_id = 1001 AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resurrected, 1, "upsert resurrects the soft-deleted row");
        let listed = list_conversations(&conn, &crate::inbox::InboxFilters::default()).unwrap();
        assert_eq!(listed.1, 1, "back in the listing");
    }

    #[test]
    fn m028_creates_conversation_threads_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='conversation_threads'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn list_conversations_exact_number_lookup() {
        // v2.0.0 (M4): ?number=N finds exactly that conversation (deep
        // links, the incident link-by-number flow); unknown numbers match
        // nothing.
        let conn = fresh_db();
        let mut conn = conn;
        let _conv_id = seed_test_data(&mut conn);
        let number: i64 = conn
            .query_row("SELECT number FROM conversations LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let (items, total) = list_conversations(
            &conn,
            &InboxFilters {
                number: Some(number),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(total, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, number);

        let (_, total) = list_conversations(
            &conn,
            &InboxFilters {
                number: Some(987_654),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(total, 0);
    }

    #[test]
    fn list_conversations_returns_empty_on_fresh_db() {
        let conn = fresh_db();
        let (items, total) = list_conversations(&conn, &InboxFilters::default()).unwrap();
        assert!(items.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn list_conversations_returns_seeded_data() {
        let mut conn = fresh_db();
        let _conv_id = seed_test_data(&mut conn);
        let (items, total) = list_conversations(&conn, &InboxFilters::default()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(total, 1);
        let item = &items[0];
        assert_eq!(item.subject.as_deref(), Some("Test subject"));
        assert_eq!(item.mailbox_name.as_deref(), Some("Test Mailbox"));
        assert_eq!(item.customer_name.as_deref(), Some("Bob Customer"));
        assert_eq!(item.assignee_name.as_deref(), Some("Alice Agent"));
    }

    #[test]
    fn list_conversations_filters_by_status() {
        let mut conn = fresh_db();
        let _conv_id = seed_test_data(&mut conn);
        // Filter for closed status — should be 0.
        let (items, total) = list_conversations(
            &conn,
            &InboxFilters {
                status: Some("closed".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(items.len(), 0);
        assert_eq!(total, 0);
        // Filter for active status — should be 1.
        let (items, total) = list_conversations(
            &conn,
            &InboxFilters {
                status: Some("active".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(total, 1);
    }

    #[test]
    fn get_conversation_returns_full_detail_with_thread() {
        let mut conn = fresh_db();
        let conv_id = seed_test_data(&mut conn);
        let detail = get_conversation(&conn, conv_id).unwrap().unwrap();
        assert_eq!(detail.id, conv_id);
        assert_eq!(detail.subject.as_deref(), Some("Test subject"));
        assert!(detail.thread.is_empty());

        // Add a thread entry directly.
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, thread_type, body, actor_type, actor_id)
             VALUES (?1, 'customer_message', 'Hello world', 'customer', NULL)",
            params![conv_id],
        )
        .unwrap();
        let detail = get_conversation(&conn, conv_id).unwrap().unwrap();
        assert_eq!(detail.thread.len(), 1);
        assert_eq!(detail.thread[0].thread_type, "customer_message");
        assert_eq!(detail.thread[0].body.as_deref(), Some("Hello world"));
    }

    #[test]
    fn get_conversation_returns_none_for_unknown_id() {
        let conn = fresh_db();
        let result = get_conversation(&conn, 99999).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn thread_type_round_trips() {
        for t in [
            ThreadType::CustomerMessage,
            ThreadType::Reply,
            ThreadType::Note,
            ThreadType::System,
        ] {
            assert_eq!(ThreadType::parse(t.as_str()), Some(t));
        }
        assert!(ThreadType::parse("unknown").is_none());
    }
}
