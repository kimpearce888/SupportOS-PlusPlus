//! Side threads — the reference sideThreadService + sideThreadRepo
//! (migration 012, plan Phase 14): internal-only collaboration threads
//! with mention resolution + immediate Notification Center fan-out.
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! Side threads are agent-only discussion threads attached to a conversation.
//! They are separate from the customer-visible conversation thread (per spec:
//! "side threads") — customers never see side-thread messages, and no method
//! here touches the Help Scout provider.
//!
//! ## Schema
//!
//! - `side_threads` — one row per thread (linked to a conversation).
//! - `side_thread_messages` — the individual messages within a thread.
//!   Each message has an optional `mentions_json` column (a JSON array of
//!   mention strings parsed from the body) so mentions can be reprocessed
//!   later (e.g. for notification replay) without re-scanning the body.
//! - `side_thread_mentions` — the RESOLVED @mentions per message (user or
//!   team, local ids) — the reference's backing store for the "mentions
//!   for me" queue.
//!
//! Mentions notify immediately (not via the sweep): the actor just typed
//! them, so the target should hear about it now. Dedup keys still make
//! re-submission idempotent.
//!
//! Per KNOWN PITFALLS: all timestamp comparisons use `julianday()`.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::NotificationType;
use crate::error::Result;
use crate::mentions;
use crate::notifications::{record_notification, NotificationInput};

/// The M006 migration: creates `side_threads` + `side_thread_messages` +
/// `side_thread_mentions` tables + indexes.
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

    CREATE TABLE IF NOT EXISTS side_thread_mentions (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        side_thread_id  INTEGER NOT NULL REFERENCES side_threads (id) ON DELETE CASCADE,
        message_id      INTEGER NOT NULL REFERENCES side_thread_messages (id) ON DELETE CASCADE,
        user_local_id   INTEGER,
        team_local_id   INTEGER,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_side_thread_mentions_user
        ON side_thread_mentions (user_local_id, created_at);

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
            ON side_thread_messages (thread_id, created_at);

        CREATE TABLE IF NOT EXISTS side_thread_mentions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            side_thread_id  INTEGER NOT NULL REFERENCES side_threads (id) ON DELETE CASCADE,
            message_id      INTEGER NOT NULL REFERENCES side_thread_messages (id) ON DELETE CASCADE,
            user_local_id   INTEGER,
            team_local_id   INTEGER,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_side_thread_mentions_user
            ON side_thread_mentions (user_local_id, created_at);",
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

// ---------------------------------------------------------------------------
// Create with the full reference schema (CL-01 / audit M14) + participants
// add with existence checks (CL-04 / sideThreadRepo.ts:173-186).
// ---------------------------------------------------------------------------

/// A parsed create-side-thread request (reference `createThread` zod
/// schema). Both the reference's camelCase wire names (`title`, `teamId`,
/// `participantUserIds`, `createdByUserId`, `firstMessage`) and the port
/// UI's snake_case names (`team_local_id`, `participant_user_ids`,
/// `created_by_user_id`, `first_message`) are accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateSideThreadInput {
    pub title: String,
    pub team_local_id: Option<i64>,
    pub participant_user_ids: Vec<i64>,
    pub created_by_user_id: Option<i64>,
    pub first_message: Option<String>,
}

/// Validation issues collected while parsing a create request — rendered as
/// `path: message` pairs like the reference's Zod 422s.
pub type ValidationIssues = Vec<(String, String)>;

fn body_i64(body: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_i64()))
}

fn body_str<'a>(body: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_str()))
}

fn body_i64_array(body: &serde_json::Value, keys: &[&str]) -> Option<Vec<i64>> {
    for k in keys {
        if let Some(v) = body.get(*k) {
            if v.is_null() {
                return Some(Vec::new());
            }
            if let Some(arr) = v.as_array() {
                return Some(arr.iter().filter_map(|x| x.as_i64()).collect::<Vec<i64>>());
            }
        }
    }
    None
}

/// Parse + validate a create-side-thread body (CL-01). Collects ALL issues
/// (missing title, wrong types) the way the reference's zod schema does.
pub fn parse_create_side_thread_input(
    body: &serde_json::Value,
) -> std::result::Result<CreateSideThreadInput, ValidationIssues> {
    let mut issues: ValidationIssues = Vec::new();
    let title = body_str(body, &["title"]).unwrap_or("").trim().to_string();
    if title.is_empty() {
        issues.push((
            "title".into(),
            "Required and must be a non-empty string.".into(),
        ));
    }
    if let Some(v) = body.get("title") {
        if !v.is_string() {
            issues.push((
                "title".into(),
                "Expected string, received other type.".into(),
            ));
        }
    }
    let mut team_local_id = body_i64(body, &["teamId", "team_local_id"]);
    if let Some(v) = body.get("teamId").or_else(|| body.get("team_local_id")) {
        if !v.is_null() && v.as_i64().is_none() {
            issues.push((
                "teamId".into(),
                "Expected number, received other type.".into(),
            ));
            team_local_id = None;
        }
    }
    let participant_user_ids =
        body_i64_array(body, &["participantUserIds", "participant_user_ids"]).unwrap_or_default();
    if let Some(v) = body
        .get("participantUserIds")
        .or_else(|| body.get("participant_user_ids"))
    {
        if !v.is_null() && v.as_array().is_none() {
            issues.push((
                "participantUserIds".into(),
                "Expected array of numbers.".into(),
            ));
        }
    }
    let mut created_by_user_id = body_i64(body, &["createdByUserId", "created_by_user_id"]);
    if let Some(v) = body
        .get("createdByUserId")
        .or_else(|| body.get("created_by_user_id"))
    {
        if !v.is_null() && v.as_i64().is_none() {
            issues.push((
                "createdByUserId".into(),
                "Expected number, received other type.".into(),
            ));
            created_by_user_id = None;
        }
    }
    let mut first_message = body_str(body, &["firstMessage", "first_message"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(v) = body
        .get("firstMessage")
        .or_else(|| body.get("first_message"))
    {
        if !v.is_null() && !v.is_string() {
            issues.push((
                "firstMessage".into(),
                "Expected string, received other type.".into(),
            ));
            first_message = None;
        }
    }
    if issues.is_empty() {
        Ok(CreateSideThreadInput {
            title,
            team_local_id,
            participant_user_ids,
            created_by_user_id,
            first_message,
        })
    } else {
        Err(issues)
    }
}

/// Cross-field existence checks the reference performs before writing
/// (CL-01: 422 on unknown participants/teams; CL-04). Returns one issue per
/// unknown reference.
pub fn check_create_references(
    conn: &Connection,
    input: &CreateSideThreadInput,
) -> ValidationIssues {
    let mut issues = ValidationIssues::new();
    if let Some(team_id) = input.team_local_id {
        let known: bool = conn
            .query_row(
                "SELECT 1 FROM teams WHERE id = ?1",
                params![team_id],
                |_| Ok(()),
            )
            .is_ok();
        if !known {
            issues.push(("teamId".into(), format!("Unknown team {team_id}.")));
        }
    }
    let unknown = unknown_user_ids(conn, &input.participant_user_ids);
    if !unknown.is_empty() {
        issues.push((
            "participantUserIds".into(),
            format!(
                "Unknown user(s): {}.",
                unknown
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    if let Some(creator) = input.created_by_user_id {
        let known: bool = conn
            .query_row(
                "SELECT 1 FROM users WHERE id = ?1",
                params![creator],
                |_| Ok(()),
            )
            .is_ok();
        if !known {
            issues.push(("createdByUserId".into(), format!("Unknown user {creator}.")));
        }
    }
    issues
}

/// The ids in `ids` that do not exist in `users` (deduplicated, order kept).
fn unknown_user_ids(conn: &Connection, ids: &[i64]) -> Vec<i64> {
    let mut unknown = Vec::new();
    for id in ids {
        if unknown.contains(id) {
            continue;
        }
        let known: bool = conn
            .query_row("SELECT 1 FROM users WHERE id = ?1", params![id], |_| Ok(()))
            .is_ok();
        if !known {
            unknown.push(*id);
        }
    }
    unknown
}

/// Create a side thread with the FULL reference schema (CL-01): title,
/// anchor team, initial participants and an optional first message (stored
/// through the mention-aware message path so the fan-out matches
/// `addMessage`). The caller has already validated `input` (conversation
/// existence + `check_create_references`).
///
/// Returns the new thread's row id.
///
/// # Errors
///
/// `Error::Sqlite` if any write fails, `Error::Other` if the first message
/// body exceeds `mentions::MAX_BODY_BYTES`.
pub fn create_side_thread_full(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    conversation_id: i64,
    input: &CreateSideThreadInput,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO side_threads (conversation_id, title, team_local_id, created_by_user_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            conversation_id,
            input.title,
            input.team_local_id,
            input.created_by_user_id
        ],
    )?;
    let thread_id = conn.last_insert_rowid();
    // Initial participants (reference createThread: participantUserIds each
    // become side_thread_participants rows, added_by = the creator).
    add_participants_checked(
        conn,
        thread_id,
        &input.participant_user_ids,
        input.created_by_user_id,
    )?;
    // The optional first message runs through the same mention-aware path
    // as POST /messages (reference sideThreadService.addMessage).
    if let Some(first) = &input.first_message {
        add_side_thread_message(conn, bus, thread_id, first, input.created_by_user_id)?;
    }
    Ok(thread_id)
}

/// Insert `side_thread_participants` rows with existence checks (CL-04,
/// reference sideThreadRepo.ts:173-186): every user must exist, otherwise
/// the whole call fails with `Error::Other` listing the unknown ids.
/// Already-present participants are skipped (the PK is
/// (side_thread_id, user_local_id) — idempotent re-adds like the
/// reference's INSERT OR IGNORE).
///
/// Returns the ids actually inserted.
///
/// # Errors
///
/// `Error::Other` listing unknown users; `Error::Sqlite` on insert failure.
pub fn add_participants_checked(
    conn: &Connection,
    thread_id: i64,
    user_ids: &[i64],
    added_by_user_local_id: Option<i64>,
) -> Result<Vec<i64>> {
    let unknown = unknown_user_ids(conn, user_ids);
    if !unknown.is_empty() {
        return Err(crate::error::Error::Other(
            format!(
                "Unknown user(s): {}",
                unknown
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into(),
        ));
    }
    let mut inserted = Vec::new();
    for uid in user_ids {
        let rows = conn.execute(
            "INSERT OR IGNORE INTO side_thread_participants
                (side_thread_id, user_local_id, added_by_user_local_id)
             VALUES (?1, ?2, ?3)",
            params![thread_id, uid, added_by_user_local_id],
        )?;
        if rows > 0 {
            inserted.push(*uid);
        }
    }
    Ok(inserted)
}

/// Add a message to a side thread — the reference
/// `SideThreadService.addMessage`: the body is scanned for mentions, the
/// RESOLVED mentions are stored in `side_thread_mentions`, and every
/// mentioned user (and every member of a mentioned team, minus the actor)
/// is notified immediately through the Notification Center funnel.
/// Re-submission is idempotent (`n:stm:{message_id}:…` dedup keys).
///
/// Per spec: side thread messages are agent-only — they are NEVER shown to
/// the customer.
///
/// Returns the new message's row id.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the insert fails, or `Error::Other` if the
/// body exceeds `mentions::MAX_BODY_BYTES`.
pub fn add_side_thread_message(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    thread_id: i64,
    body: &str,
    author_user_id: Option<i64>,
) -> Result<i64> {
    // Scan for mentions up front so we can store them in the row.
    // Fail-fast if the body is too large.
    let raw_mentions = mentions::scan_for_mentions(body)?;
    let mentions_json = if raw_mentions.is_empty() {
        None
    } else {
        let display_strings: Vec<String> = raw_mentions
            .iter()
            .map(mentions::Mention::display)
            .collect();
        Some(serde_json::to_string(&display_strings).map_err(|e| {
            crate::error::Error::Config(format!("mentions_json serialization failed: {e}"))
        })?)
    };

    conn.execute(
        "INSERT INTO side_thread_messages (thread_id, body, author_user_id, mentions_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![thread_id, body, author_user_id, mentions_json],
    )?;
    let message_id = conn.last_insert_rowid();

    // Mention fan-out (immediate — the actor just typed it). The thread's
    // conversation facts resolve local-first (side threads may carry either
    // the local or the remote conversation id, depending on the caller).
    let thread: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT conversation_id, title FROM side_threads WHERE id = ?1",
            params![thread_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    if let Some((conversation_ref, thread_title)) = thread {
        let conv: Option<(i64, Option<i64>)> = conn
            .query_row(
                "SELECT id, number FROM conversations
                 WHERE id = ?1 OR remote_id = ?1
                 ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END LIMIT 1",
                params![conversation_ref],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let (conversation_id, conversation_number) = conv
            .map(|(id, number)| (Some(id), number))
            .unwrap_or((None, None));
        let thread_title = thread_title.unwrap_or_else(|| "side thread".into());
        let number_for_title = conversation_number.unwrap_or(conversation_ref);
        let author_name = author_user_id
            .and_then(|id| {
                mentions::build_mention_directory(conn)
                    .ok()
                    .and_then(|d| d.display_by_user.get(&id).cloned())
            })
            .unwrap_or_else(|| "Someone".into());
        let trimmed_body = trim_200(body);

        let directory = mentions::build_mention_directory(conn)?;
        for m in mentions::parse_mentions(body, &directory)? {
            // Persist the resolved mention (the "mentions for me" store).
            let _ = conn.execute(
                "INSERT INTO side_thread_mentions
                    (side_thread_id, message_id, user_local_id, team_local_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![thread_id, message_id, m.user_local_id, m.team_local_id],
            );
            if let Some(user) = m.user_local_id {
                if Some(user) == author_user_id {
                    continue;
                }
                let _ = record_notification(
                    conn,
                    bus,
                    &NotificationInput {
                        notification_type: NotificationType::Mentioned,
                        title: format!(
                            "{author_name} mentioned you in \"{thread_title}\" (#{number_for_title})"
                        ),
                        body: Some(trimmed_body.clone()),
                        target_user_local_id: Some(user),
                        actor_user_local_id: author_user_id,
                        conversation_id,
                        conversation_number,
                        side_thread_id: Some(thread_id),
                        dedup_key: format!("n:stm:{message_id}:{user}"),
                        ..Default::default()
                    },
                );
            }
            if let Some(team) = m.team_local_id {
                for member in mentions::team_members(conn, team)?.into_iter() {
                    if Some(member) == author_user_id {
                        continue;
                    }
                    let _ = record_notification(
                        conn,
                        bus,
                        &NotificationInput {
                            notification_type: NotificationType::TeamMentioned,
                            title: format!(
                                "{author_name} mentioned @{} (you are a member) in \"{thread_title}\" (#{number_for_title})",
                                m.display
                            ),
                            body: Some(trimmed_body.clone()),
                            target_user_local_id: Some(member),
                            actor_user_local_id: author_user_id,
                            conversation_id,
                            conversation_number,
                            side_thread_id: Some(thread_id),
                            dedup_key: format!("n:stm:{message_id}:team{team}:{member}"),
                            ..Default::default()
                        },
                    );
                }
            }
        }
    }
    Ok(message_id)
}

/// The reference's 200-char body cap: `body.length > 200 ? slice(0,199)+'…'`.
fn trim_200(body: &str) -> String {
    if body.chars().count() > 200 {
        let cut: String = body.chars().take(199).collect();
        format!("{cut}…")
    } else {
        body.to_string()
    }
}

/// One "mentions for me" row — the reference
/// `sideThreadRepo.mentionsForUser` entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SideThreadMentionForUser {
    pub message_id: i64,
    pub thread_id: i64,
    pub thread_title: Option<String>,
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub author: Option<String>,
    pub body: String,
    pub created_at: String,
}

/// The side-thread mentions targeting one user — "mentions for me" source
/// #2 (the reference `sideThreadRepo.mentionsForUser`).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn mentions_for_user(conn: &Connection, me: i64) -> Result<Vec<SideThreadMentionForUser>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.thread_id, t.title, t.conversation_id, m.body, m.created_at,
                (SELECT TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, ''))
                   FROM users u WHERE u.id = m.author_user_id) AS author
         FROM side_thread_mentions stm
         JOIN side_thread_messages m ON m.id = stm.message_id
         JOIN side_threads t ON t.id = stm.side_thread_id
         WHERE stm.user_local_id = ?1
         ORDER BY m.created_at DESC LIMIT 100",
    )?;
    let rows = stmt
        .query_map(params![me], |r| {
            Ok(SideThreadMentionForUser {
                message_id: r.get(0)?,
                thread_id: r.get(1)?,
                thread_title: r.get(2)?,
                conversation_id: r.get(3)?,
                conversation_number: None,
                author: r.get(6)?,
                body: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Resolve the conversation numbers (local-first; side threads may carry
    // either key form).
    let mut resolved = rows;
    for m in &mut resolved {
        m.conversation_number = conn
            .query_row(
                "SELECT number FROM conversations
                 WHERE id = ?1 OR remote_id = ?1
                 ORDER BY CASE WHEN id = ?1 THEN 0 ELSE 1 END LIMIT 1",
                params![m.conversation_id],
                |r| r.get(0),
            )
            .ok();
    }
    Ok(resolved)
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

// ---------------------------------------------------------------------------
// MAIN list/detail payloads (audit B2 + N1 / plan item CL-02).
//
// The reference serves `GET /api/conversations/:id/side-threads` and
// `GET /api/side-threads/:id` from `SideThreadRepository.listThreads` /
// `getThread` (src/server/database/repositories/sideThreadRepo.ts:51-104):
// a summary row per thread (title/status/message_count/team_name/
// conversation_number/last_message_at …) and a detail object that adds
// participants + messages with their resolved mentions. The port's storage
// keeps a few divergent column names (created_by_user_id, thread_id,
// author_user_id — audit DB-04); they are mapped to the reference payload
// field names at this query boundary.
// ---------------------------------------------------------------------------

/// One list row — the reference `mapThread` payload
/// (sideThreadRepo.ts:249-265).
#[derive(Debug, Clone, Serialize)]
pub struct SideThreadSummary {
    pub id: i64,
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub title: String,
    pub team_local_id: Option<i64>,
    pub team_name: Option<String>,
    pub status: String,
    /// Stored as `created_by_user_id` in the port schema; served under the
    /// reference payload field name.
    pub created_by_user_local_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
    pub message_count: i64,
    pub last_message_at: Option<String>,
}

/// One participant row of a detail payload (sideThreadRepo.ts:86-101).
#[derive(Debug, Clone, Serialize)]
pub struct SideThreadParticipant {
    pub user_local_id: i64,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub mention: Option<String>,
    pub added_at: String,
    pub added_by_user_local_id: Option<i64>,
}

/// One resolved @mention attached to a message (sideThreadRepo.ts:118-123).
#[derive(Debug, Clone, Serialize)]
pub struct SideThreadMentionRef {
    pub user_local_id: Option<i64>,
    pub team_local_id: Option<i64>,
}

/// One message row of a detail payload (sideThreadRepo.ts:124-133).
#[derive(Debug, Clone, Serialize)]
pub struct SideThreadMessageItem {
    pub id: i64,
    /// Stored as `thread_id` in the port schema; served under the reference
    /// payload field name.
    pub side_thread_id: i64,
    /// Stored as `author_user_id` in the port schema.
    pub author_user_local_id: Option<i64>,
    pub author_first_name: Option<String>,
    pub author_last_name: Option<String>,
    pub body: String,
    pub created_at: String,
    pub mentions: Vec<SideThreadMentionRef>,
}

/// The detail payload — the reference `getThread` return
/// (sideThreadRepo.ts:70-104): the summary fields plus participants and
/// messages.
#[derive(Debug, Clone, Serialize)]
pub struct SideThreadDetail {
    pub id: i64,
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub title: String,
    pub team_local_id: Option<i64>,
    pub team_name: Option<String>,
    pub status: String,
    pub created_by_user_local_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
    pub message_count: i64,
    pub last_message_at: Option<String>,
    pub participants: Vec<SideThreadParticipant>,
    pub messages: Vec<SideThreadMessageItem>,
}

/// Row shape of the shared summary SELECT — the reference SQL
/// (sideThreadRepo.ts:52-64 listThreads / 71-84 getThread) adapted to the
/// port's column names.
struct ThreadSummaryRow {
    id: i64,
    conversation_id: i64,
    title: String,
    team_local_id: Option<i64>,
    team_name: Option<String>,
    status: String,
    created_by_user_local_id: Option<i64>,
    created_at: String,
    updated_at: String,
    resolved_at: Option<String>,
    conversation_number: Option<i64>,
    message_count: i64,
    last_message_at: Option<String>,
}

impl ThreadSummaryRow {
    /// Indices follow THREAD_SUMMARY_SELECT's column order:
    /// id, conversation_id, title, team_local_id, status, created_by_user_id,
    /// created_at, updated_at, resolved_at, conversation_number, team_name,
    /// message_count, last_message_at.
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            conversation_id: r.get(1)?,
            title: r.get(2)?,
            team_local_id: r.get(3)?,
            status: r.get(4)?,
            created_by_user_local_id: r.get(5)?,
            created_at: r.get(6)?,
            updated_at: r.get(7)?,
            resolved_at: r.get(8)?,
            conversation_number: r.get(9)?,
            team_name: r.get(10)?,
            message_count: r.get(11)?,
            last_message_at: r.get(12)?,
        })
    }
}

/// The summary SELECT shared by list + detail (reference sideThreadRepo.ts
/// 52-64: thread columns + conversation number + team name + message
/// count/last-message subqueries). Open threads come first, newest update
/// first — `ORDER BY st.status = 'resolved', st.updated_at DESC` in the
/// reference; `julianday()` per this crate's KNOWN PITFALLS (mixed
/// timestamp formats in the port columns).
const THREAD_SUMMARY_SELECT: &str = "SELECT st.id, st.conversation_id, st.title, st.team_local_id,
       st.status, st.created_by_user_id, st.created_at, st.updated_at, st.resolved_at,
       c.number AS conversation_number, t.name AS team_name,
       (SELECT COUNT(*) FROM side_thread_messages m WHERE m.thread_id = st.id) AS message_count,
       (SELECT MAX(m.created_at) FROM side_thread_messages m WHERE m.thread_id = st.id) AS last_message_at
FROM side_threads st
JOIN conversations c ON c.id = st.conversation_id
LEFT JOIN teams t ON t.id = st.team_local_id";

/// List a conversation's side threads in the reference list payload shape
/// (sideThreadRepo.ts:51-68). `conversation_id` is the LOCAL conversation id
/// (conversations.id), like the reference.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_side_thread_summaries(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Vec<SideThreadSummary>> {
    let mut stmt = conn.prepare(&format!(
        "{THREAD_SUMMARY_SELECT}
         WHERE st.conversation_id = ?1
         ORDER BY (st.status = 'resolved') ASC, julianday(st.updated_at) DESC"
    ))?;
    let rows = stmt
        .query_map(params![conversation_id], ThreadSummaryRow::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|r| SideThreadSummary {
            id: r.id,
            conversation_id: r.conversation_id,
            conversation_number: r.conversation_number,
            title: r.title,
            team_local_id: r.team_local_id,
            team_name: r.team_name,
            status: r.status,
            created_by_user_local_id: r.created_by_user_local_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
            resolved_at: r.resolved_at,
            message_count: r.message_count,
            last_message_at: r.last_message_at,
        })
        .collect())
}

/// Fetch one side thread in the reference detail payload shape
/// (sideThreadRepo.ts:70-104): summary fields + participants + messages
/// with resolved mentions. Returns `None` when the thread does not exist
/// (the route answers 404, like the reference).
///
/// # Errors
///
/// Returns `Error::Sqlite` if a query fails.
pub fn get_side_thread_detail(
    conn: &Connection,
    thread_id: i64,
) -> Result<Option<SideThreadDetail>> {
    use rusqlite::OptionalExtension;

    let mut stmt = conn.prepare(&format!("{THREAD_SUMMARY_SELECT} WHERE st.id = ?1"))?;
    let row = stmt
        .query_row(params![thread_id], ThreadSummaryRow::read)
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };

    // Participants (reference sideThreadRepo.ts:86-101).
    let mut stmt = conn.prepare(
        "SELECT p.user_local_id, u.first_name, u.last_name, u.mention,
                p.added_at, p.added_by_user_local_id
         FROM side_thread_participants p JOIN users u ON u.id = p.user_local_id
         WHERE p.side_thread_id = ?1
         ORDER BY julianday(p.added_at) ASC, p.user_local_id ASC",
    )?;
    let participants = stmt
        .query_map(params![thread_id], |r| {
            Ok(SideThreadParticipant {
                user_local_id: r.get(0)?,
                first_name: r.get(1)?,
                last_name: r.get(2)?,
                mention: r.get(3)?,
                added_at: r.get(4)?,
                added_by_user_local_id: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Messages + author names (reference sideThreadRepo.ts:106-134).
    let mut stmt = conn.prepare(
        "SELECT m.id, m.thread_id, m.author_user_id, m.body, m.created_at,
                u.first_name AS author_first_name, u.last_name AS author_last_name
         FROM side_thread_messages m LEFT JOIN users u ON u.id = m.author_user_id
         WHERE m.thread_id = ?1
         ORDER BY julianday(m.created_at) ASC, m.id ASC",
    )?;
    let messages = stmt
        .query_map(params![thread_id], |r| {
            Ok(SideThreadMessageItem {
                id: r.get(0)?,
                side_thread_id: r.get(1)?,
                author_user_local_id: r.get(2)?,
                author_first_name: r.get(5)?,
                author_last_name: r.get(6)?,
                body: r.get(3)?,
                created_at: r.get(4)?,
                mentions: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Resolved mentions, grouped per message (reference
    // sideThreadRepo.ts:115-123).
    let mut by_message: std::collections::HashMap<i64, Vec<SideThreadMentionRef>> =
        std::collections::HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT message_id, user_local_id, team_local_id
         FROM side_thread_mentions WHERE side_thread_id = ?1",
    )?;
    let mention_rows = stmt
        .query_map(params![thread_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                SideThreadMentionRef {
                    user_local_id: r.get(1)?,
                    team_local_id: r.get(2)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (message_id, m) in mention_rows {
        by_message.entry(message_id).or_default().push(m);
    }
    let mut messages = messages;
    for m in &mut messages {
        if let Some(mentions) = by_message.get(&m.id) {
            m.mentions = mentions.clone();
        }
    }

    Ok(Some(SideThreadDetail {
        id: row.id,
        conversation_id: row.conversation_id,
        conversation_number: row.conversation_number,
        title: row.title,
        team_local_id: row.team_local_id,
        team_name: row.team_name,
        status: row.status,
        created_by_user_local_id: row.created_by_user_local_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
        resolved_at: row.resolved_at,
        message_count: row.message_count,
        last_message_at: row.last_message_at,
        participants,
        messages,
    }))
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
        // The canonical boot chain — the mention fan-out reads the users /
        // teams / team_members / conversations mirrors and writes
        // reference-shaped notifications.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    /// The standard demo-ish identity fixture: Alex (id 1, @alex),
    /// Priya (id 2, @priya), team "Tier 1" (id 1) with both members.
    fn seed_identities(conn: &Connection) {
        conn.execute(
            "INSERT INTO users (remote_id, first_name, last_name, mention, user_type)
             VALUES (1001, 'Alex', 'Rivera', 'alex', 'user'),
                    (1002, 'Priya', 'Nair', 'priya', 'user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO teams (remote_id, name) VALUES (501, 'Tier 1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO team_members (team_id, user_id) VALUES (1, 1), (1, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (105000, 5001, 'active', 201, 3001)",
            [],
        )
        .unwrap();
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

    // ---- CL-01: create with the full schema ------------------------------

    #[test]
    fn parse_create_input_accepts_reference_camel_case() {
        let body = serde_json::json!({
            "title": "  Escalation ",
            "teamId": 7,
            "participantUserIds": [1, 2],
            "createdByUserId": 1,
            "firstMessage": " looking into this "
        });
        let input = parse_create_side_thread_input(&body).unwrap();
        assert_eq!(input.title, "Escalation");
        assert_eq!(input.team_local_id, Some(7));
        assert_eq!(input.participant_user_ids, vec![1, 2]);
        assert_eq!(input.created_by_user_id, Some(1));
        assert_eq!(input.first_message.as_deref(), Some("looking into this"));
    }

    #[test]
    fn parse_create_input_accepts_port_snake_case() {
        let body = serde_json::json!({
            "title": "Escalation",
            "team_local_id": 7,
            "participant_user_ids": [2],
            "created_by_user_id": 1,
            "first_message": null
        });
        let input = parse_create_side_thread_input(&body).unwrap();
        assert_eq!(input.team_local_id, Some(7));
        assert_eq!(input.participant_user_ids, vec![2]);
        assert_eq!(input.first_message, None);
    }

    #[test]
    fn parse_create_input_collects_missing_and_mistyped_issues() {
        // Missing title + wrong-typed team + non-array participants.
        let body = serde_json::json!({
            "teamId": "seven",
            "participantUserIds": 5
        });
        let issues = parse_create_side_thread_input(&body).unwrap_err();
        let joined = issues
            .iter()
            .map(|(p, m)| format!("{p}: {m}"))
            .collect::<Vec<_>>()
            .join("; ");
        assert!(joined.contains("title: Required"), "{joined}");
        assert!(joined.contains("teamId: Expected number"), "{joined}");
        assert!(
            joined.contains("participantUserIds: Expected array"),
            "{joined}"
        );
    }

    #[test]
    fn check_create_references_flags_unknown_team_participants_and_creator() {
        let conn = fresh_db();
        seed_identities(&conn);
        let input = CreateSideThreadInput {
            title: "Escalation".into(),
            team_local_id: Some(999),
            participant_user_ids: vec![1, 77, 88],
            created_by_user_id: Some(66),
            first_message: None,
        };
        let issues = check_create_references(&conn, &input);
        let joined = issues
            .iter()
            .map(|(p, m)| format!("{p}: {m}"))
            .collect::<Vec<_>>()
            .join("; ");
        assert!(joined.contains("teamId: Unknown team 999"), "{joined}");
        assert!(
            joined.contains("participantUserIds: Unknown user(s): 77, 88"),
            "{joined}"
        );
        assert!(
            joined.contains("createdByUserId: Unknown user 66"),
            "{joined}"
        );
    }

    #[test]
    fn create_side_thread_full_persists_everything() {
        let conn = fresh_db();
        seed_identities(&conn);
        // The seeded conversation (remote 105000) lands as the local id.
        let conv_id: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 105000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let input = CreateSideThreadInput {
            title: "Pricing escalation".into(),
            team_local_id: Some(1),
            participant_user_ids: vec![1, 2],
            created_by_user_id: Some(1),
            first_message: Some("Kicking this off - @priya can you take a look?".into()),
        };
        let id = create_side_thread_full(&conn, None, conv_id, &input).unwrap();

        // Thread row: title + team + creator.
        let (title, team, creator): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT title, team_local_id, created_by_user_id FROM side_threads WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(title, "Pricing escalation");
        assert_eq!(team, Some(1));
        assert_eq!(creator, Some(1));

        // Participants: both users, added_by = the creator.
        let mut rows = conn
            .prepare(
                "SELECT user_local_id, added_by_user_local_id FROM side_thread_participants
                 WHERE side_thread_id = ?1 ORDER BY user_local_id",
            )
            .unwrap()
            .query_map(params![id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })
            .unwrap()
            .flatten()
            .collect::<Vec<_>>();
        rows.sort();
        assert_eq!(rows, vec![(1, Some(1)), (2, Some(1))]);

        // First message stored through the mention-aware path with the
        // creator as author + the @priya mention resolved.
        let (body, author, mentions): (String, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT body, author_user_id, mentions_json FROM side_thread_messages
                 WHERE thread_id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(body, "Kicking this off - @priya can you take a look?");
        assert_eq!(author, Some(1));
        assert!(mentions.is_some(), "the @priya mention must be recorded");

        // The detail payload serves all of it.
        let detail = get_side_thread_detail(&conn, id).unwrap().unwrap();
        assert_eq!(detail.title, "Pricing escalation");
        assert_eq!(detail.team_name.as_deref(), Some("Tier 1"));
        assert_eq!(detail.participants.len(), 2);
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(detail.messages[0].body, body);
    }

    #[test]
    fn create_side_thread_full_without_optionals_stores_minimal_row() {
        let conn = fresh_db();
        seed_identities(&conn);
        let conv_id: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 105000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let input = CreateSideThreadInput {
            title: "Quick huddle".into(),
            team_local_id: None,
            participant_user_ids: vec![],
            created_by_user_id: None,
            first_message: None,
        };
        let id = create_side_thread_full(&conn, None, conv_id, &input).unwrap();
        let n_messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM side_thread_messages WHERE thread_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_messages, 0, "no first message -> no message row");
    }

    // ---- CL-04: participants add with existence checks --------------------

    #[test]
    fn add_participants_checked_inserts_rows() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(1)).unwrap();
        let inserted = add_participants_checked(&conn, thread_id, &[1, 2], Some(1)).unwrap();
        assert_eq!(inserted, vec![1, 2]);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM side_thread_participants WHERE side_thread_id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn add_participants_checked_rejects_unknown_users_without_partial_writes() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(1)).unwrap();
        let err = add_participants_checked(&conn, thread_id, &[1, 55], None).unwrap_err();
        assert!(err.to_string().contains("Unknown user"), "{err}");
        // No partial write: the known user 1 was NOT inserted.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM side_thread_participants WHERE side_thread_id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn add_participants_checked_skips_duplicates() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(1)).unwrap();
        add_participants_checked(&conn, thread_id, &[1], None).unwrap();
        // Re-adding the same user is a no-op; a new user in the same call is
        // still inserted.
        let inserted = add_participants_checked(&conn, thread_id, &[1, 2], Some(2)).unwrap();
        assert_eq!(inserted, vec![2]);
        let (added_by_2,): (Option<i64>,) = conn
            .query_row(
                "SELECT added_by_user_local_id FROM side_thread_participants
                 WHERE side_thread_id = ?1 AND user_local_id = 2",
                params![thread_id],
                |r| Ok((r.get(0)?,)),
            )
            .unwrap();
        assert_eq!(added_by_2, Some(2));
    }

    // ---- add_side_thread_message --------------------------------------------

    #[test]
    fn add_side_thread_message_returns_row_id() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let msg_id =
            add_side_thread_message(&conn, None, thread_id, "Hello team", Some(42)).unwrap();
        assert!(msg_id > 0);
    }

    #[test]
    fn add_side_thread_message_stores_body_and_author() {
        let conn = fresh_db();
        let thread_id = create_side_thread(&conn, 1001, Some(42)).unwrap();
        let msg_id =
            add_side_thread_message(&conn, None, thread_id, "Heads up @alice", Some(42)).unwrap();
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
            None,
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
            add_side_thread_message(&conn, None, thread_id, "No mentions here", Some(42)).unwrap();
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
        let result = add_side_thread_message(&conn, None, thread_id, &big, Some(42));
        assert!(result.is_err(), "oversized body must be rejected");
    }

    // ---- mention fan-out (reference sideThreadService.addMessage) ----------

    #[test]
    fn user_mention_in_side_thread_notifies_immediately() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(2)).unwrap();
        conn.execute(
            "UPDATE side_threads SET title = 'Billing escalation' WHERE id = ?1",
            params![thread_id],
        )
        .unwrap();

        let msg_id = add_side_thread_message(
            &conn,
            None,
            thread_id,
            "@alex can you take the refund part?",
            Some(2),
        )
        .unwrap();

        let (title, body, target, actor, side_thread, dedup): (
            String,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            String,
        ) = conn
            .query_row(
                "SELECT title, body, target_user_id, actor_user_local_id, side_thread_id, dedup_key
                 FROM notifications WHERE type = 'mentioned'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            title,
            "Priya Nair mentioned you in \"Billing escalation\" (#5001)"
        );
        assert_eq!(body.as_deref(), Some("@alex can you take the refund part?"));
        assert_eq!(target, Some(1), "Alex was mentioned");
        assert_eq!(actor, Some(2), "Priya wrote the message");
        assert_eq!(side_thread, Some(thread_id));
        assert_eq!(dedup, format!("n:stm:{msg_id}:1"));

        // The resolved mention row backs the "mentions for me" queue.
        let mine = mentions_for_user(&conn, 1).unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].message_id, msg_id);
        assert_eq!(mine[0].thread_title.as_deref(), Some("Billing escalation"));
        assert_eq!(mine[0].conversation_number, Some(5001));
        assert_eq!(mine[0].author.as_deref(), Some("Priya Nair"));
    }

    #[test]
    fn team_mention_in_side_thread_notifies_every_member_except_the_actor() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(1)).unwrap();
        conn.execute(
            "UPDATE side_threads SET title = 'Spike huddle' WHERE id = ?1",
            params![thread_id],
        )
        .unwrap();

        // Alex mentions the whole Tier 1 team; he is a member himself.
        let msg_id =
            add_side_thread_message(&conn, None, thread_id, "@Tier 1 heads up", Some(1)).unwrap();

        let rows: Vec<(Option<i64>, String)> = conn
            .prepare(
                "SELECT target_user_id, title FROM notifications WHERE type = 'team_mentioned'",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 1, "only Priya — the actor is skipped");
        assert_eq!(rows[0].0, Some(2));
        assert_eq!(
            rows[0].1,
            "Alex Rivera mentioned @Tier 1 (you are a member) in \"Spike huddle\" (#5001)"
        );
        let dedup: String = conn
            .query_row(
                "SELECT dedup_key FROM notifications WHERE type = 'team_mentioned'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dedup, format!("n:stm:{msg_id}:team1:2"));
    }

    #[test]
    fn self_mention_and_unknown_mentions_never_notify() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(1)).unwrap();
        add_side_thread_message(
            &conn,
            None,
            thread_id,
            "@alex talking to myself and @nobody",
            Some(1),
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "self-mention + unknown token → no notifications");
    }

    #[test]
    fn mention_fan_out_emits_notification_received_sse() {
        let conn = fresh_db();
        seed_identities(&conn);
        let thread_id = create_side_thread(&conn, 105000, Some(2)).unwrap();
        let bus = crate::http::EventBus::new(8);
        let mut rx = bus.subscribe();
        add_side_thread_message(&conn, Some(&bus), thread_id, "@alex ping", Some(2)).unwrap();
        let event = rx.blocking_recv().expect("SSE event emitted");
        assert!(
            matches!(
                event,
                crate::events::ServerEvent::NotificationReceived(ref e)
                    if e.kind == "mentioned"
            ),
            "got {event:?}"
        );
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
        add_side_thread_message(&conn, None, t1, "msg 1 in t1", Some(42)).unwrap();
        add_side_thread_message(&conn, None, t1, "msg 2 in t1", Some(42)).unwrap();
        add_side_thread_message(&conn, None, t2, "msg in t2", Some(43)).unwrap();

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
        let m1 = add_side_thread_message(&conn, None, thread_id, "first", Some(42)).unwrap();
        // Set explicit timestamps out of order.
        conn.execute(
            "UPDATE side_thread_messages SET created_at = '2026-01-01T10:00:00Z' WHERE id = ?1",
            params![m1],
        )
        .unwrap();
        let m2 = add_side_thread_message(&conn, None, thread_id, "second", Some(42)).unwrap();
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
        add_side_thread_message(&conn, None, thread_id, "msg 1", Some(42)).unwrap();
        add_side_thread_message(&conn, None, thread_id, "msg 2", Some(42)).unwrap();
        add_side_thread_message(&conn, None, thread_id, "msg 3", Some(42)).unwrap();
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
