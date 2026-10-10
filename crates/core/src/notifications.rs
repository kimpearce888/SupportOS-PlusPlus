//! Notification Center — the reference `notificationRepo` (migration 012)
//! plus the sweep's `notify()` funnel.
//!
//! Reference: src/server/database/repositories/notificationRepo.ts +
//! the `notify()` method of notificationSweep.ts.
//!
//! Design decisions (ported verbatim):
//! - INSERT ... ON CONFLICT(dedup_key) DO NOTHING + reading changes: the
//!   caller learns whether the row is NEW, so sweep re-runs and re-syncs are
//!   idempotent by construction.
//! - Targeting: `target_user_local_id` = a specific user, NULL = broadcast.
//!   List/read queries take the acting user and match
//!   "target IS NULL OR target = me".
//! - Severity/title are produced SERVER-SIDE from closed notification types;
//!   user text (subjects, note excerpts) only ever lands in title/body as
//!   DATA, never as markup.
//! - Preferences are checked BEFORE insert: a disabled type produces no row
//!   at all (not a hidden row), so the store stays honest.
//! - A genuinely new row emits the `notification-received` SSE event (the
//!   reference's serverEventBus fan-out, wired through the port's EventBus).
//! - Time formats: created_at/read_at use `datetime('now')`-style space
//!   format (SQLite default). Retention comparisons use julianday() which
//!   accepts both that and ISO.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::NotificationType;
use crate::error::Result;

/// The M005 migration: creates the `notifications` table + indexes.
/// Fresh databases get the full reference (migration 012) column set;
/// pre-existing ones gain the missing columns via ALTER below.
pub const M005_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS notifications (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        type            TEXT NOT NULL,
        severity        TEXT NOT NULL DEFAULT 'info',
        title           TEXT NOT NULL DEFAULT '',
        body            TEXT,
        target_user_id  INTEGER,
        actor_user_local_id INTEGER,
        conversation_id INTEGER,
        conversation_number INTEGER,
        customer_local_id   INTEGER,
        issue_id        INTEGER,
        campaign_id     INTEGER,
        job_id          INTEGER,
        side_thread_id  INTEGER,
        dedup_key       TEXT NOT NULL DEFAULT '',
        payload         TEXT,
        read_at         TEXT,
        created_at      TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_dedup
        ON notifications (dedup_key);
    CREATE INDEX IF NOT EXISTS idx_notifications_target_unread
        ON notifications (target_user_id, read_at);
    CREATE INDEX IF NOT EXISTS idx_notifications_type_created_at
        ON notifications (type, created_at);
    CREATE INDEX IF NOT EXISTS idx_notifications_created_at
        ON notifications (created_at);

    UPDATE app_state SET schema_version = 5 WHERE id = 1;
"#;

/// Apply M005 migration. Idempotent: CREATE TABLE IF NOT EXISTS plus
/// best-effort ALTERs for databases created by the older (narrow) M005.
pub fn apply_m005(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notifications (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            type            TEXT NOT NULL,
            severity        TEXT NOT NULL DEFAULT 'info',
            title           TEXT NOT NULL DEFAULT '',
            body            TEXT,
            target_user_id  INTEGER,
            actor_user_local_id INTEGER,
            conversation_id INTEGER,
            conversation_number INTEGER,
            customer_local_id   INTEGER,
            issue_id        INTEGER,
            campaign_id     INTEGER,
            job_id          INTEGER,
            side_thread_id  INTEGER,
            dedup_key       TEXT NOT NULL DEFAULT '',
            payload         TEXT,
            read_at         TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_dedup
            ON notifications (dedup_key);
        CREATE INDEX IF NOT EXISTS idx_notifications_target_unread
            ON notifications (target_user_id, read_at);
        CREATE INDEX IF NOT EXISTS idx_notifications_type_created_at
            ON notifications (type, created_at);
        CREATE INDEX IF NOT EXISTS idx_notifications_created_at
            ON notifications (created_at);",
    )?;
    // Reference (012) columns older port databases lack. ALTER defaults are
    // required for NOT NULL on SQLite; the reference's own breadth pass does
    // the same (db_breadth "notifications (012). ADAPTED").
    for (column, decl) in [
        ("title", "TEXT NOT NULL DEFAULT ''"),
        ("body", "TEXT"),
        ("actor_user_local_id", "INTEGER"),
        ("conversation_number", "INTEGER"),
        ("customer_local_id", "INTEGER"),
        ("issue_id", "INTEGER"),
        ("campaign_id", "INTEGER"),
        ("job_id", "INTEGER"),
        ("side_thread_id", "INTEGER"),
        ("dedup_key", "TEXT NOT NULL DEFAULT ''"),
    ] {
        let sql = format!("ALTER TABLE notifications ADD COLUMN {column} {decl}");
        if let Err(e) = conn.execute(&sql, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(crate::error::Error::Sqlite(e));
            }
        }
    }
    // schema_version bump — idempotent.
    let _ = conn.execute("UPDATE app_state SET schema_version = 5 WHERE id = 1", []);
    Ok(())
}

/// The insert input — the reference `notificationRepo.insert` parameter.
#[derive(Debug, Clone)]
pub struct NotificationInput<'a> {
    /// The closed-vocabulary notification type.
    pub notification_type: NotificationType,
    /// Severity override (the incident service varies it per event);
    /// `None` = the type's map default (`NotificationType::severity`).
    pub severity: Option<&'a str>,
    /// Server-composed headline (truncated to 300 chars on insert).
    pub title: String,
    /// Optional body (truncated to 2000 chars on insert).
    pub body: Option<String>,
    /// Who the notification is for; `None` = broadcast.
    pub target_user_local_id: Option<i64>,
    /// Who/what caused it, when known (`None` = system).
    pub actor_user_local_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub conversation_number: Option<i64>,
    pub customer_local_id: Option<i64>,
    pub issue_id: Option<i64>,
    pub campaign_id: Option<i64>,
    pub job_id: Option<i64>,
    pub side_thread_id: Option<i64>,
    /// Stable identity of the underlying fact — re-notification is
    /// impossible for the same key.
    pub dedup_key: String,
}

impl Default for NotificationInput<'_> {
    /// The neutral skeleton for struct-update syntax — every real caller
    /// sets `notification_type`, `title` and `dedup_key` explicitly.
    fn default() -> Self {
        Self {
            notification_type: NotificationType::CustomerReplied,
            severity: None,
            title: String::new(),
            body: None,
            target_user_local_id: None,
            actor_user_local_id: None,
            conversation_id: None,
            conversation_number: None,
            customer_local_id: None,
            issue_id: None,
            campaign_id: None,
            job_id: None,
            side_thread_id: None,
            dedup_key: String::new(),
        }
    }
}

/// A notification row — the reference `NotificationRecord` shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotificationRecord {
    pub id: i64,
    #[serde(rename = "type")]
    pub notification_type: NotificationType,
    /// 'info' | 'warning' | 'critical'
    pub severity: String,
    pub title: String,
    pub body: Option<String>,
    pub target_user_local_id: Option<i64>,
    pub actor_user_local_id: Option<i64>,
    pub conversation_id: Option<i64>,
    pub conversation_number: Option<i64>,
    pub customer_local_id: Option<i64>,
    pub issue_id: Option<i64>,
    pub campaign_id: Option<i64>,
    pub job_id: Option<i64>,
    pub side_thread_id: Option<i64>,
    pub created_at: String,
    pub read_at: Option<String>,
}

/// The list result — the reference `NotificationListResult`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NotificationListResult {
    pub notifications: Vec<NotificationRecord>,
    pub total: i64,
    pub unread: i64,
}

/// Validate that `type_str` is one of the 15 catalog notification types.
fn parse_type(type_str: &str) -> Option<NotificationType> {
    NotificationType::ALL
        .iter()
        .copied()
        .find(|t| t.as_str() == type_str)
}

/// The connected Help Scout user, falling back to the first synced user
/// (reference `NotificationSweep.meUserLocalId`).
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the user lookup fails.
pub fn me_user_local_id(conn: &Connection) -> Result<Option<i64>> {
    // The setting may be stored raw ("1001") or JSON-quoted ("\"1001\"") —
    // the reference reads it through json_extract, which accepts both.
    let raw = crate::settings::get_string(conn, "me_remote_id")?;
    if let Some(raw) = raw {
        let trimmed = raw.trim_matches('"');
        if let Ok(remote_id) = trimmed.parse::<i64>() {
            let row: Option<i64> = conn
                .query_row(
                    "SELECT id FROM users WHERE remote_id = ?1",
                    params![remote_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(id) = row {
                return Ok(Some(id));
            }
        }
    }
    let fallback: Option<i64> = conn
        .query_row(
            "SELECT id FROM users WHERE deleted_at IS NULL ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if fallback.is_none() {
        // Older schemas without users.deleted_at — first user, unfiltered.
        return Ok(conn
            .query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |r| r.get(0))
            .ok());
    }
    Ok(fallback)
}

/// Effective enabled state for a type (the reference `prefFor`): the
/// per-installation settings key `notifications.<type>.enabled`, falling
/// back to the type's default. Missing row = default (all on).
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the settings read fails.
pub fn pref_for(conn: &Connection, notif_type: NotificationType) -> Result<bool> {
    let key = format!("notifications.{}.enabled", notif_type.as_str());
    crate::settings::get_bool(conn, &key, notif_type.default_enabled())
}

/// Insert with dedup — the reference `notificationRepo.insert`. Returns the
/// row only when it is genuinely new (`None` = deduped).
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the insert fails, or
/// [`crate::error::Error::Other`] for an unknown type (defensive — the
/// typed enum makes this unreachable in practice).
pub fn insert(
    conn: &Connection,
    input: &NotificationInput<'_>,
) -> Result<Option<NotificationRecord>> {
    let severity = input
        .severity
        .unwrap_or_else(|| input.notification_type.severity());
    let inserted = conn.execute(
        "INSERT INTO notifications
            (type, severity, title, body, target_user_id, actor_user_local_id,
             conversation_id, conversation_number, customer_local_id, issue_id,
             campaign_id, job_id, side_thread_id, dedup_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(dedup_key) DO NOTHING",
        params![
            input.notification_type.as_str(),
            severity,
            truncate_chars(&input.title, 300),
            input.body.as_deref().map(|b| truncate_chars(b, 2000)),
            input.target_user_local_id,
            input.actor_user_local_id,
            input.conversation_id,
            input.conversation_number,
            input.customer_local_id,
            input.issue_id,
            input.campaign_id,
            input.job_id,
            input.side_thread_id,
            input.dedup_key,
        ],
    )?;
    if inserted == 0 {
        return Ok(None);
    }
    get(conn, conn.last_insert_rowid())
}

/// Fetch one notification row.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the query fails.
pub fn get(conn: &Connection, id: i64) -> Result<Option<NotificationRecord>> {
    let row = conn
        .query_row(
            "SELECT id, type, severity, title, body, target_user_id, actor_user_local_id,
                    conversation_id, conversation_number, customer_local_id, issue_id,
                    campaign_id, job_id, side_thread_id, created_at, read_at
             FROM notifications WHERE id = ?1",
            params![id],
            row_to_notification,
        )
        .ok();
    Ok(row)
}

/// Record a notification — the reference `NotificationSweep.notify`:
/// preference check BEFORE insert (a disabled type produces no row), deduped
/// insert, and a `notification-received` SSE event on the live insert.
/// Returns the row only when it is genuinely new.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if any query fails.
pub fn record_notification(
    conn: &Connection,
    bus: Option<&crate::http::EventBus>,
    input: &NotificationInput<'_>,
) -> Result<Option<NotificationRecord>> {
    if !pref_for(conn, input.notification_type)? {
        return Ok(None);
    }
    let Some(created) = insert(conn, input)? else {
        return Ok(None);
    };
    if let Some(bus) = bus {
        let unread = unread_count(conn, me_user_local_id(conn)?)?;
        crate::http::event_bus::notify_notification_received(
            bus,
            created.id,
            created.notification_type.as_str(),
            &created.severity,
            &created.title,
            created.conversation_id,
            created.conversation_number,
            created.customer_local_id,
            created.target_user_local_id,
            unread.max(0) as u32,
        );
    }
    Ok(Some(created))
}

/// Options for [`list`] — the reference `list(opts)`.
#[derive(Debug, Clone, Default)]
pub struct ListOptions<'a> {
    /// Acting user; NULL rows (broadcast) always match.
    pub me_user_local_id: Option<i64>,
    pub unread_only: bool,
    pub notif_type: Option<&'a str>,
    pub limit: i64,
    pub offset: i64,
}

/// List notifications for the acting user — the reference `list`:
/// `(target IS NULL OR target = me) [AND read_at IS NULL] [AND type = ?]`,
/// newest first, with the total + unread counters.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the queries fail.
pub fn list(conn: &Connection, opts: &ListOptions<'_>) -> Result<NotificationListResult> {
    let limit = opts.limit.clamp(1, 200);
    let offset = opts.offset.max(0);
    let me = opts.me_user_local_id.unwrap_or(-1);
    let mut where_sql = String::from("(n.target_user_id IS NULL OR n.target_user_id = ?1)");
    if opts.unread_only {
        where_sql.push_str(" AND n.read_at IS NULL");
    }
    if let Some(t) = opts.notif_type {
        where_sql.push_str(&format!(" AND n.type = '{}'", t.replace('\'', "''")));
    }
    let rows: Vec<NotificationRecord> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT n.id, n.type, n.severity, n.title, n.body, n.target_user_id,
                    n.actor_user_local_id, n.conversation_id, n.conversation_number,
                    n.customer_local_id, n.issue_id, n.campaign_id, n.job_id,
                    n.side_thread_id, n.created_at, n.read_at
             FROM notifications n WHERE {where_sql}
             ORDER BY n.created_at DESC, n.id DESC LIMIT ?2 OFFSET ?3"
        ))?;
        let mapped = stmt
            .query_map(params![me, limit, offset], row_to_notification)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM notifications n WHERE {where_sql}"),
        params![me],
        |r| r.get(0),
    )?;
    let unread: i64 = conn.query_row(
        "SELECT COUNT(*) FROM notifications
          WHERE (target_user_id IS NULL OR target_user_id = ?1) AND read_at IS NULL",
        params![me],
        |r| r.get(0),
    )?;
    Ok(NotificationListResult {
        notifications: rows,
        total,
        unread,
    })
}

/// Notifications of type 'mentioned'/'team_mentioned' targeting the user —
/// the "mentions for me" queue source #1.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the query fails.
pub fn mentions_for_me(conn: &Connection, me: i64, limit: i64) -> Result<Vec<NotificationRecord>> {
    let limit = limit.clamp(1, 200);
    let mut stmt = conn.prepare(
        "SELECT id, type, severity, title, body, target_user_id, actor_user_local_id,
                conversation_id, conversation_number, customer_local_id, issue_id,
                campaign_id, job_id, side_thread_id, created_at, read_at
         FROM notifications
         WHERE type IN ('mentioned', 'team_mentioned') AND target_user_id = ?1
         ORDER BY created_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![me, limit], row_to_notification)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count unread notifications for the acting user (broadcasts included).
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the query fails.
pub fn unread_count(conn: &Connection, me: Option<i64>) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM notifications
         WHERE (target_user_id IS NULL OR target_user_id = ?1) AND read_at IS NULL",
        params![me.unwrap_or(-1)],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Mark one notification read/unread for the acting user — the reference
/// `markRead`: honors `{read: boolean}`, only touches rows visible to the
/// acting user, returns whether anything changed (unknown id → false → 404).
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the update fails.
pub fn mark_read(conn: &Connection, id: i64, me: Option<i64>, read: bool) -> Result<bool> {
    let stamp = if read { "datetime('now')" } else { "NULL" };
    let rows = conn.execute(
        &format!(
            "UPDATE notifications SET read_at = {stamp}
             WHERE id = ?1 AND (target_user_id IS NULL OR target_user_id = ?2)"
        ),
        params![id, me.unwrap_or(-1)],
    )?;
    Ok(rows > 0)
}

/// Mark every unread notification visible to the acting user as read —
/// user-scoped by construction (the reference `markAllRead`).
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the update fails.
pub fn mark_all_read(conn: &Connection, me: Option<i64>) -> Result<usize> {
    let rows = conn.execute(
        "UPDATE notifications SET read_at = datetime('now')
         WHERE read_at IS NULL AND (target_user_id IS NULL OR target_user_id = ?1)",
        params![me.unwrap_or(-1)],
    )?;
    Ok(rows)
}

/// Prune read+old or plain-old notifications (space-format cutoff) — the
/// reference `pruneOlderThan`: `created_at < cutoff OR (read AND
/// read_at < cutoff)`. Old UNREAD notifications prune too; a recent read
/// one survives until its read_at crosses the window.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if the delete fails.
pub fn prune_older_than(conn: &Connection, cutoff: &str) -> Result<usize> {
    let rows = conn.execute(
        "DELETE FROM notifications
         WHERE julianday(created_at) < julianday(?1)
            OR (read_at IS NOT NULL AND julianday(read_at) < julianday(?1))",
        params![cutoff],
    )?;
    Ok(rows)
}

/// Convert a SQLite row to a `NotificationRecord`.
fn row_to_notification(r: &rusqlite::Row<'_>) -> rusqlite::Result<NotificationRecord> {
    let type_str: String = r.get(1)?;
    let notification_type = parse_type(&type_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unknown notification type: {type_str}").into(),
        )
    })?;
    Ok(NotificationRecord {
        id: r.get(0)?,
        notification_type,
        severity: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        target_user_local_id: r.get(5)?,
        actor_user_local_id: r.get(6)?,
        conversation_id: r.get(7)?,
        conversation_number: r.get(8)?,
        customer_local_id: r.get(9)?,
        issue_id: r.get(10)?,
        campaign_id: r.get(11)?,
        job_id: r.get(12)?,
        side_thread_id: r.get(13)?,
        created_at: r.get(14)?,
        read_at: r.get(15)?,
    })
}

/// The reference `trim`-on-insert: cap a string to `max` chars, appending an
/// ellipsis when truncation happens (`slice(0, max - 1) + '…'`).
fn truncate_chars(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() > max {
        let cut: String = t.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        t.to_string()
    }
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
        // The canonical boot chain — tests must exercise the REAL schema
        // (notifications with the reference's dedup_key + unique index),
        // never a partial one.
        crate::bootstrap::apply_all(&mut conn).unwrap();
        // DB-06: the notification FKs — the users/conversation the shared
        // `input` fixture stamps (target 42, conversation 7; user 43 for
        // the not-visible-to cases) must exist. OR IGNORE: tests that seed
        // their own identities (the me_user_local_id family) stay in
        // control after a DELETE FROM users.
        conn.execute_batch(
            "INSERT OR IGNORE INTO users (id, remote_id, first_name)
             VALUES (42, 1001, 'Me'), (43, 1002, 'Other');
             INSERT OR IGNORE INTO mailboxes (id, remote_id, name) VALUES (1, 1, 'Support');
             INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (1, 1, 'Cust');
             INSERT OR IGNORE INTO conversations (id, remote_id, number, status, mailbox_local_id, customer_local_id)
             VALUES (7, 900007, 5001, 'active', 1, 1);",
        )
        .unwrap();
        conn
    }

    fn input(kind: NotificationType, dedup: &str) -> NotificationInput<'static> {
        NotificationInput {
            notification_type: kind,
            severity: None,
            title: "Test title".into(),
            body: Some("Test body".into()),
            target_user_local_id: Some(42),
            actor_user_local_id: None,
            conversation_id: Some(7),
            conversation_number: Some(5001),
            customer_local_id: None,
            issue_id: None,
            campaign_id: None,
            job_id: None,
            side_thread_id: None,
            dedup_key: dedup.into(),
        }
    }

    // ---- migration ----------------------------------------------------------

    #[test]
    fn m005_creates_reference_shaped_notifications_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        for column in [
            "title",
            "body",
            "actor_user_local_id",
            "conversation_number",
            "customer_local_id",
            "issue_id",
            "campaign_id",
            "job_id",
            "side_thread_id",
            "dedup_key",
        ] {
            let present: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('notifications') WHERE name = '{column}'"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "column {column} must exist");
        }
        let dedup_index: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_notifications_dedup'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dedup_index, 1, "the dedup unique index is load-bearing");
    }

    #[test]
    fn m005_is_idempotent() {
        let conn = fresh_db();
        apply_m005(&conn).unwrap();
    }

    // ---- insert / record_notification ---------------------------------------

    #[test]
    fn insert_returns_the_row_only_when_new() {
        let conn = fresh_db();
        let first = insert(&conn, &input(NotificationType::SlaBreach, "n:test:1"))
            .unwrap()
            .expect("first insert is new");
        assert_eq!(first.notification_type, NotificationType::SlaBreach);
        assert_eq!(first.severity, "critical");
        assert_eq!(first.title, "Test title");
        assert!(first.read_at.is_none());
        let second = insert(&conn, &input(NotificationType::SlaBreach, "n:test:1")).unwrap();
        assert!(second.is_none(), "same dedup key → deduped");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn insert_severity_override_beats_the_type_default() {
        let conn = fresh_db();
        let created = insert(
            &conn,
            &NotificationInput {
                notification_type: NotificationType::IncidentUpdate,
                severity: Some("critical"),
                ..input(NotificationType::IncidentUpdate, "n:test:sev")
            },
        )
        .unwrap()
        .expect("new");
        assert_eq!(created.severity, "critical");
        assert_eq!(
            NotificationType::IncidentUpdate.severity(),
            "warning",
            "override differs from the default — the override must win"
        );
    }

    #[test]
    fn insert_truncates_title_and_body() {
        let conn = fresh_db();
        let created = insert(
            &conn,
            &NotificationInput {
                title: "x".repeat(400),
                body: Some("y".repeat(2500)),
                ..input(NotificationType::KnownIssueDetected, "n:test:trunc")
            },
        )
        .unwrap()
        .expect("new");
        assert_eq!(created.title.chars().count(), 300);
        assert!(created.title.ends_with('…'));
        assert_eq!(created.body.unwrap().chars().count(), 2000);
    }

    #[test]
    fn record_notification_gates_on_preference_before_insert() {
        let conn = fresh_db();
        crate::settings::set_bool(&conn, "notifications.sla_breach.enabled", false).unwrap();
        let created = record_notification(
            &conn,
            None,
            &input(NotificationType::SlaBreach, "n:test:gate"),
        )
        .unwrap();
        assert!(created.is_none(), "disabled type → no row at all");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the store stays honest — not a hidden row");

        // Re-enabling produces the row on the next notify (same dedup key).
        crate::settings::set_bool(&conn, "notifications.sla_breach.enabled", true).unwrap();
        let created = record_notification(
            &conn,
            None,
            &input(NotificationType::SlaBreach, "n:test:gate"),
        )
        .unwrap();
        assert!(created.is_some());
    }

    #[test]
    fn record_notification_emits_notification_received_sse() {
        let conn = fresh_db();
        // The SSE badge counts for the ME user (notificationSweep.ts:73) —
        // fresh_db already seeds me = local user 42 (remote 1001).
        crate::settings::set_string(&conn, "me_remote_id", "1001").unwrap();
        let bus = crate::http::EventBus::new(8);
        let mut rx = bus.subscribe();
        let created = record_notification(
            &conn,
            Some(&bus),
            &input(NotificationType::CustomerReplied, "n:test:sse"),
        )
        .unwrap()
        .expect("new");
        let event = rx.blocking_recv().expect("SSE event emitted");
        match event {
            crate::events::ServerEvent::NotificationReceived(e) => {
                assert_eq!(e.id, created.id);
                assert_eq!(e.kind, "customer_replied");
                assert_eq!(e.severity, "info");
                assert_eq!(e.title, "Test title");
                assert_eq!(e.conversation_id, Some(7));
                assert_eq!(e.conversation_number, Some(5001));
                assert_eq!(e.target_user_local_id, Some(42));
                assert_eq!(e.unread_count, 1);
            }
            other => panic!("expected NotificationReceived, got {other:?}"),
        }
        // A deduped insert must NOT emit a second event.
        let dup = record_notification(
            &conn,
            Some(&bus),
            &input(NotificationType::CustomerReplied, "n:test:sse"),
        )
        .unwrap();
        assert!(dup.is_none());
        assert!(rx.try_recv().is_err(), "no second SSE event");
    }

    #[test]
    fn record_notification_without_bus_is_silent() {
        let conn = fresh_db();
        assert!(record_notification(
            &conn,
            None,
            &input(NotificationType::SyncFailure, "n:test:nobus")
        )
        .unwrap()
        .is_some());
    }

    // ---- list ---------------------------------------------------------------

    #[test]
    fn list_scopes_to_me_plus_broadcasts_with_counters() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::Mentioned, "n:t:1")).unwrap();
        let mut other = input(NotificationType::Mentioned, "n:t:2");
        other.target_user_local_id = Some(43);
        insert(&conn, &other).unwrap();
        let mut broadcast = input(NotificationType::IssueSpike, "n:t:3");
        broadcast.target_user_local_id = None;
        insert(&conn, &broadcast).unwrap();

        let result = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(42),
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.notifications.len(), 2, "mine + broadcast");
        assert_eq!(result.total, 2);
        assert_eq!(result.unread, 2);

        // The other user sees their own row + the broadcast, not mine.
        let result = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(43),
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.notifications.len(), 2);
    }

    #[test]
    fn list_unread_only_and_type_filters() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::SlaRisk, "n:f:1")).unwrap();
        insert(&conn, &input(NotificationType::SlaBreach, "n:f:2")).unwrap();
        mark_read(&conn, 1, Some(42), true).unwrap();

        let unread_only = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(42),
                unread_only: true,
                limit: 50,
                offset: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(unread_only.notifications.len(), 1);
        assert_eq!(
            unread_only.notifications[0].notification_type,
            NotificationType::SlaBreach
        );
        // unread only counts unread rows even when the page filter differs.
        assert_eq!(unread_only.unread, 1);

        let typed = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(42),
                unread_only: false,
                notif_type: Some("sla_risk"),
                limit: 50,
                offset: 0,
            },
        )
        .unwrap();
        assert_eq!(typed.notifications.len(), 1);
        assert_eq!(
            typed.notifications[0].notification_type,
            NotificationType::SlaRisk
        );
        assert_eq!(typed.total, 1);
        // The unread counter ignores the type filter (reference behavior).
        assert_eq!(typed.unread, 1);
    }

    #[test]
    fn list_offset_pages_within_the_filter() {
        let conn = fresh_db();
        for i in 0..5 {
            insert(
                &conn,
                &input(NotificationType::JobFailure, &format!("n:o:{i}")),
            )
            .unwrap();
        }
        let page = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(42),
                limit: 2,
                offset: 4,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page.notifications.len(), 1, "5 rows, offset 4 → 1 left");
        assert_eq!(page.total, 5);
    }

    #[test]
    fn list_clamps_limit_into_the_reference_window() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::JobFailure, "n:c:1")).unwrap();
        let zero = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(42),
                limit: 0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(zero.notifications.len(), 1, "limit clamps up to 1");
        let huge = list(
            &conn,
            &ListOptions {
                me_user_local_id: Some(42),
                limit: 9999,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(huge.notifications.len(), 1, "limit clamps down to 200");
    }

    // ---- mentions queue -----------------------------------------------------

    #[test]
    fn mentions_for_me_returns_mention_kinds_targeting_me() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::Mentioned, "n:m:1")).unwrap();
        let mut team = input(NotificationType::TeamMentioned, "n:m:2");
        team.side_thread_id = Some(9);
        insert(&conn, &team).unwrap();
        let mut other_user = input(NotificationType::Mentioned, "n:m:3");
        other_user.target_user_local_id = Some(43);
        insert(&conn, &other_user).unwrap();
        insert(&conn, &input(NotificationType::SlaBreach, "n:m:4")).unwrap();

        let mine = mentions_for_me(&conn, 42, 100).unwrap();
        assert_eq!(mine.len(), 2, "mentioned + team_mentioned only");
        assert!(mine.iter().all(|n| n.target_user_local_id == Some(42)));
    }

    // ---- mark read / read-all ----------------------------------------------

    #[test]
    fn mark_read_honors_read_flag_and_visibility() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::Mentioned, "n:r:1")).unwrap();
        assert!(mark_read(&conn, 1, Some(42), true).unwrap());
        let row = get(&conn, 1).unwrap().unwrap();
        assert!(row.read_at.is_some());

        // {read: false} un-marks (the reference sets read_at = NULL).
        assert!(mark_read(&conn, 1, Some(42), false).unwrap());
        let row = get(&conn, 1).unwrap().unwrap();
        assert!(row.read_at.is_none());

        // A user the notification is not visible to cannot mark it.
        assert!(!mark_read(&conn, 1, Some(43), true).unwrap());

        // Unknown id → false (the route 404s on this).
        assert!(!mark_read(&conn, 999, Some(42), true).unwrap());
    }

    #[test]
    fn mark_all_read_is_user_scoped() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::Mentioned, "n:a:1")).unwrap();
        insert(&conn, &input(NotificationType::Mentioned, "n:a:2")).unwrap();
        let mut other = input(NotificationType::SlaBreach, "n:a:3");
        other.target_user_local_id = Some(43);
        insert(&conn, &other).unwrap();

        let marked = mark_all_read(&conn, Some(42)).unwrap();
        assert_eq!(marked, 2, "only rows visible to user 42");
        let still_unread = unread_count(&conn, Some(43)).unwrap();
        assert_eq!(still_unread, 1, "user 43's row stays unread");
    }

    // ---- retention ----------------------------------------------------------

    #[test]
    fn prune_older_than_deletes_old_unread_and_old_read_rows() {
        let conn = fresh_db();
        insert(&conn, &input(NotificationType::Mentioned, "n:p:1")).unwrap();
        let old_unread = insert(&conn, &input(NotificationType::Mentioned, "n:p:2"))
            .unwrap()
            .unwrap();
        let old_read = insert(&conn, &input(NotificationType::Mentioned, "n:p:3"))
            .unwrap()
            .unwrap();
        let recent_read = insert(&conn, &input(NotificationType::Mentioned, "n:p:4"))
            .unwrap()
            .unwrap();
        // 40 days old everywhere; recent_read was read just now.
        conn.execute(
            "UPDATE notifications SET created_at = datetime('now', '-40 days') WHERE id IN (?1, ?2)",
            params![old_unread.id, old_read.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE notifications SET read_at = datetime('now', '-40 days') WHERE id = ?1",
            params![old_read.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE notifications SET read_at = datetime('now') WHERE id = ?1",
            params![recent_read.id],
        )
        .unwrap();

        let removed = prune_older_than(&conn, "2026-01-01 00:00:00").unwrap();
        assert_eq!(removed, 0, "cutoff in the past removes nothing");

        // Cutoff = 30 days ago: old unread AND old read prune; recent read
        // survives (its read_at is inside the window).
        let cutoff: String = conn
            .query_row("SELECT datetime('now', '-30 days')", [], |r| r.get(0))
            .unwrap();
        let removed = prune_older_than(&conn, &cutoff).unwrap();
        assert_eq!(removed, 2, "old unread prunes too (reference predicate)");
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 2);
    }

    // ---- me resolution ------------------------------------------------------

    #[test]
    fn me_user_local_id_resolves_me_remote_id_then_first_user() {
        let conn = fresh_db();
        // This family owns the users table (the fixture's shared identities
        // would otherwise take the first-user slot).
        conn.execute("DELETE FROM users", []).unwrap();
        conn.execute(
            "INSERT INTO users (remote_id, first_name) VALUES (1001, 'Alex')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (remote_id, first_name) VALUES (1002, 'Priya')",
            [],
        )
        .unwrap();
        // No setting → first synced user.
        assert_eq!(me_user_local_id(&conn).unwrap(), Some(1));
        // me_remote_id set (raw form) → that user's LOCAL id.
        crate::settings::set_string(&conn, "me_remote_id", "1002").unwrap();
        assert_eq!(me_user_local_id(&conn).unwrap(), Some(2));
        // JSON-quoted form (the reference's settings storage) also parses.
        crate::settings::set_string(&conn, "me_remote_id", "\"1002\"").unwrap();
        assert_eq!(me_user_local_id(&conn).unwrap(), Some(2));
    }

    #[test]
    fn me_user_local_id_is_none_without_users() {
        let conn = fresh_db();
        conn.execute("DELETE FROM users", []).unwrap();
        assert_eq!(me_user_local_id(&conn).unwrap(), None);
    }
}
