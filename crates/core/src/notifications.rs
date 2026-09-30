//! Notification Center — M005 migration + record/list/mark-as-read API (M4-T04).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! Per CHANGELOG v2.1.x: "Notification Center v2 (15 types, per-type
//! preferences, retention pruning)."
//!
//! Per CHANGELOG v1.7.x bug fix: "Notification sweep fired too early on first
//! run, spamming the user. Fix: cursors must not init until the first sync
//! settles." The guardrail itself lives in M4-T05 (the sweep engine); this
//! module provides the data layer that the sweep writes to.
//!
//! The 15 notification types are the closed vocabulary
//! `NotificationType::ALL` from the catalog crate (single source of truth).
//! `record_notification()` validates the type against this enum (rejects
//! unknown types) and copies `severity` from `NotificationType::severity()`.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::NotificationType;
use crate::error::{Error, Result};

/// The M005 migration: creates the `notifications` table + indexes.
pub const M005_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS notifications (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        type            TEXT NOT NULL,
        severity        TEXT NOT NULL,
        target_user_id  INTEGER,
        conversation_id INTEGER,
        payload         TEXT,
        read_at         TEXT,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_notifications_target_user_read_at
        ON notifications (target_user_id, read_at);
    CREATE INDEX IF NOT EXISTS idx_notifications_type_created_at
        ON notifications (type, created_at);
    CREATE INDEX IF NOT EXISTS idx_notifications_created_at
        ON notifications (created_at);

    UPDATE app_state SET schema_version = 5 WHERE id = 1;
"#;

/// Apply M005 migration. Idempotent (CREATE TABLE IF NOT EXISTS).
pub fn apply_m005(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS notifications (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            type            TEXT NOT NULL,
            severity        TEXT NOT NULL,
            target_user_id  INTEGER,
            conversation_id INTEGER,
            payload         TEXT,
            read_at         TEXT,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_notifications_target_user_read_at
            ON notifications (target_user_id, read_at);
        CREATE INDEX IF NOT EXISTS idx_notifications_type_created_at
            ON notifications (type, created_at);
        CREATE INDEX IF NOT EXISTS idx_notifications_created_at
            ON notifications (created_at);",
    )?;
    // schema_version bump — idempotent.
    let _ = conn.execute("UPDATE app_state SET schema_version = 5 WHERE id = 1", []);
    Ok(())
}

/// A notification row, mirroring the `notifications` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    /// The row id (assigned by SQLite on insert).
    pub id: Option<i64>,
    /// The notification type (validated against `NotificationType::ALL`).
    #[serde(rename = "type")]
    pub notification_type: NotificationType,
    /// The severity bucket (copied from `NotificationType::severity()`).
    pub severity: String,
    /// The agent who should see this notification. `None` for "all agents".
    pub target_user_id: Option<i64>,
    /// The conversation the notification is about. `None` for system-wide.
    pub conversation_id: Option<i64>,
    /// The JSON payload (triggering event details).
    pub payload: Option<String>,
    /// When the user marked the notification as read. `None` if unread.
    pub read_at: Option<String>,
    /// When the notification was created (ISO-8601 UTC).
    pub created_at: String,
}

/// Validate that `type_str` is one of the 15 catalog notification types.
/// Returns the typed `NotificationType` on success, `Error::Other` on failure.
fn parse_type(type_str: &str) -> Result<NotificationType> {
    for t in NotificationType::ALL {
        if t.as_str() == type_str {
            return Ok(t);
        }
    }
    Err(Error::Other(
        format!("unknown notification type: {type_str}").into(),
    ))
}

/// Record a notification. Validates the type against the catalog's
/// `NotificationType::ALL` (single source of truth) — unknown types are
/// rejected. Severity is copied from `NotificationType::severity()`.
///
/// # Errors
///
/// Returns `Error::Other` if `notification_type` is not one of the 15 catalog
/// types, or `Error::Sqlite` if the insert fails.
pub fn record_notification(
    conn: &Connection,
    notification_type: &NotificationType,
    target_user_id: Option<i64>,
    conversation_id: Option<i64>,
    payload: Option<&str>,
) -> Result<i64> {
    // Validate by construction: the caller passes a typed NotificationType,
    // so the type IS in the catalog. But we still write `as_str()` to the DB
    // and the severity from `severity()`.
    let type_str = notification_type.as_str();
    let severity = notification_type.severity();
    conn.execute(
        "INSERT INTO notifications (type, severity, target_user_id, conversation_id, payload)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![type_str, severity, target_user_id, conversation_id, payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Mark a notification as read. Sets `read_at` to the current time.
/// Returns `true` if the row was updated, `false` if no unread notification
/// matched the id (already read, or doesn't exist).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn mark_as_read(conn: &Connection, notification_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE notifications SET read_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1 AND read_at IS NULL",
        params![notification_id],
    )?;
    Ok(rows > 0)
}

/// List unread notifications for a user. Returns the most recent first.
/// Includes notifications with `target_user_id IS NULL` (broadcast).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_unread_for_user(
    conn: &Connection,
    user_id: i64,
    limit: u32,
) -> Result<Vec<Notification>> {
    let limit = limit.clamp(1, 200);
    let mut stmt = conn.prepare(
        "SELECT id, type, severity, target_user_id, conversation_id, payload, read_at, created_at
         FROM notifications
         WHERE read_at IS NULL
           AND (target_user_id = ?1 OR target_user_id IS NULL)
         ORDER BY created_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![user_id, limit], row_to_notification)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// List all notifications for a user (read + unread). Returns the most
/// recent first.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_for_user(conn: &Connection, user_id: i64, limit: u32) -> Result<Vec<Notification>> {
    let limit = limit.clamp(1, 200);
    let mut stmt = conn.prepare(
        "SELECT id, type, severity, target_user_id, conversation_id, payload, read_at, created_at
         FROM notifications
         WHERE target_user_id = ?1 OR target_user_id IS NULL
         ORDER BY created_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![user_id, limit], row_to_notification)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count unread notifications for a user. The UI badge uses this.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn count_unread_for_user(conn: &Connection, user_id: i64) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM notifications
         WHERE read_at IS NULL
           AND (target_user_id = ?1 OR target_user_id IS NULL)",
        params![user_id],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

/// Convert a SQLite row to a `Notification`. Parses `type` against the
/// catalog enum (rejects unknown types — shouldn't happen if writes went
/// through `record_notification`, but defensive against manual inserts).
fn row_to_notification(r: &rusqlite::Row<'_>) -> rusqlite::Result<Notification> {
    let type_str: String = r.get(1)?;
    let notification_type = parse_type(&type_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Notification {
        id: r.get(0)?,
        notification_type,
        severity: r.get(2)?,
        target_user_id: r.get(3)?,
        conversation_id: r.get(4)?,
        payload: r.get(5)?,
        read_at: r.get(6)?,
        created_at: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::apply_m003;
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
        conn
    }

    // ---- M005 migration -----------------------------------------------------

    #[test]
    fn m005_creates_notifications_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m005_is_idempotent() {
        let conn = fresh_db();
        // Re-applying M005 should not error (tables/indexes already exist).
        apply_m005(&conn).unwrap();
    }

    #[test]
    fn m005_creates_target_user_read_at_index() {
        let conn = fresh_db();
        // The index is required for efficient "unread for user X" queries.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_notifications_target_user_read_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // ---- record_notification ------------------------------------------------

    #[test]
    fn record_notification_inserts_row_with_correct_severity() {
        let conn = fresh_db();
        let id = record_notification(
            &conn,
            &NotificationType::SlaBreach,
            Some(42),
            Some(1001),
            Some(r#"{"reason":"breached"}"#),
        )
        .unwrap();
        assert!(id > 0);

        let (type_str, severity, target, conv, payload, read_at): (
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT type, severity, target_user_id, conversation_id, payload, read_at
                 FROM notifications WHERE id = ?1",
                params![id],
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
        assert_eq!(type_str, "sla_breach");
        assert_eq!(severity, "critical", "SlaBreach.severity() = 'critical'");
        assert_eq!(target, Some(42));
        assert_eq!(conv, Some(1001));
        assert_eq!(payload.as_deref(), Some(r#"{"reason":"breached"}"#));
        assert!(read_at.is_none(), "newly inserted notification is unread");
    }

    #[test]
    fn record_notification_all_15_types_round_trip() {
        let conn = fresh_db();
        for t in NotificationType::ALL {
            let id = record_notification(&conn, &t, Some(42), None, None).unwrap();
            assert!(id > 0, "failed to insert {t:?}");

            let (type_str, severity): (String, String) = conn
                .query_row(
                    "SELECT type, severity FROM notifications WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(type_str, t.as_str());
            assert_eq!(severity, t.severity());
        }

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 15);
    }

    #[test]
    fn record_notification_severity_matches_catalog_severity_map() {
        let conn = fresh_db();
        for t in NotificationType::ALL {
            let id = record_notification(&conn, &t, None, None, None).unwrap();
            let severity: String = conn
                .query_row(
                    "SELECT severity FROM notifications WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(severity, t.severity());
            assert!(
                matches!(severity.as_str(), "info" | "warning" | "critical"),
                "invalid severity: {severity}"
            );
        }
    }

    #[test]
    fn record_notification_with_null_target_user_is_broadcast() {
        let conn = fresh_db();
        let id =
            record_notification(&conn, &NotificationType::IssueSpike, None, None, None).unwrap();
        let target: Option<i64> = conn
            .query_row(
                "SELECT target_user_id FROM notifications WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(target.is_none(), "broadcast notification has no target");
    }

    // ---- mark_as_read -------------------------------------------------------

    #[test]
    fn mark_as_read_sets_read_at_and_returns_true() {
        let conn = fresh_db();
        let id =
            record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();

        let updated = mark_as_read(&conn, id).unwrap();
        assert!(updated, "first mark_as_read should update");

        let read_at: Option<String> = conn
            .query_row(
                "SELECT read_at FROM notifications WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            read_at.is_some(),
            "read_at should be set after mark_as_read"
        );
    }

    #[test]
    fn mark_as_read_is_idempotent() {
        let conn = fresh_db();
        let id =
            record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();

        let first = mark_as_read(&conn, id).unwrap();
        assert!(first);
        let second = mark_as_read(&conn, id).unwrap();
        assert!(
            !second,
            "second mark_as_read should return false (no unread row)"
        );
    }

    #[test]
    fn mark_as_read_returns_false_for_nonexistent_id() {
        let conn = fresh_db();
        let updated = mark_as_read(&conn, 9999).unwrap();
        assert!(!updated);
    }

    // ---- list_unread_for_user -----------------------------------------------

    #[test]
    fn list_unread_for_user_returns_only_unread() {
        let conn = fresh_db();
        // 3 unread for user 42, 1 read, 1 for a different user.
        let id1 =
            record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();
        let _id2 =
            record_notification(&conn, &NotificationType::SlaRisk, Some(42), None, None).unwrap();
        let _id3 =
            record_notification(&conn, &NotificationType::SlaBreach, Some(42), None, None).unwrap();
        let id4 =
            record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();
        let _id5 =
            record_notification(&conn, &NotificationType::Mentioned, Some(43), None, None).unwrap();

        mark_as_read(&conn, id1).unwrap();
        mark_as_read(&conn, id4).unwrap();

        let unread = list_unread_for_user(&conn, 42, 50).unwrap();
        assert_eq!(unread.len(), 2, "only 2 unread remain for user 42");
        for n in &unread {
            assert!(n.read_at.is_none());
            assert_eq!(n.target_user_id, Some(42));
        }
    }

    #[test]
    fn list_unread_includes_broadcast_notifications() {
        let conn = fresh_db();
        // A broadcast notification (target_user_id = NULL) should be visible
        // to all users.
        record_notification(&conn, &NotificationType::IssueSpike, None, None, None).unwrap();
        record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();

        let unread = list_unread_for_user(&conn, 42, 50).unwrap();
        assert_eq!(
            unread.len(),
            2,
            "broadcast + targeted = 2 unread for user 42"
        );
    }

    #[test]
    fn list_unread_is_limited_and_ordered_by_created_at_desc() {
        let conn = fresh_db();
        // Insert 5 notifications.
        for t in [
            NotificationType::Mentioned,
            NotificationType::SlaRisk,
            NotificationType::SlaBreach,
            NotificationType::IssueSpike,
            NotificationType::SyncFailure,
        ] {
            record_notification(&conn, &t, Some(42), None, None).unwrap();
            // Tiny delay so created_at timestamps differ.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let limited = list_unread_for_user(&conn, 42, 3).unwrap();
        assert_eq!(limited.len(), 3, "limit is honored");

        // The most-recent-first ordering is verified by checking the
        // timestamps are descending (later → earlier).
        for w in limited.windows(2) {
            assert!(
                w[0].created_at >= w[1].created_at,
                "expected DESC order: {} vs {}",
                w[0].created_at,
                w[1].created_at
            );
        }
    }

    #[test]
    fn list_unread_for_user_with_no_notifications_returns_empty() {
        let conn = fresh_db();
        let unread = list_unread_for_user(&conn, 42, 50).unwrap();
        assert!(unread.is_empty());
    }

    // ---- list_for_user (all, including read) --------------------------------

    #[test]
    fn list_for_user_includes_read_and_unread() {
        let conn = fresh_db();
        let id1 =
            record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();
        let _id2 =
            record_notification(&conn, &NotificationType::SlaRisk, Some(42), None, None).unwrap();
        mark_as_read(&conn, id1).unwrap();

        let all = list_for_user(&conn, 42, 50).unwrap();
        assert_eq!(all.len(), 2, "list_for_user includes read + unread");
    }

    // ---- count_unread_for_user ----------------------------------------------

    #[test]
    fn count_unread_for_user_includes_broadcast() {
        let conn = fresh_db();
        record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();
        record_notification(&conn, &NotificationType::IssueSpike, None, None, None).unwrap();
        record_notification(&conn, &NotificationType::SyncFailure, Some(43), None, None).unwrap();

        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 2, "1 targeted + 1 broadcast = 2 unread for user 42");
    }

    #[test]
    fn count_unread_excludes_read() {
        let conn = fresh_db();
        let id1 =
            record_notification(&conn, &NotificationType::Mentioned, Some(42), None, None).unwrap();
        record_notification(&conn, &NotificationType::SlaRisk, Some(42), None, None).unwrap();
        mark_as_read(&conn, id1).unwrap();

        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 1, "1 unread + 1 read = 1 unread");
    }

    // ---- parse_type (defensive) ---------------------------------------------

    #[test]
    fn parse_type_rejects_unknown_type() {
        let result = parse_type("not_a_real_type");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown notification type"), "got: {msg}");
    }

    #[test]
    fn parse_type_accepts_all_15_catalog_types() {
        for t in NotificationType::ALL {
            let parsed = parse_type(t.as_str()).unwrap();
            assert_eq!(parsed, t);
        }
    }

    // ---- Notification struct + serde ----------------------------------------

    #[test]
    fn notification_serializes_with_type_field() {
        let n = Notification {
            id: Some(7),
            notification_type: NotificationType::SlaBreach,
            severity: "critical".into(),
            target_user_id: Some(42),
            conversation_id: Some(1001),
            payload: Some(r#"{"reason":"breached"}"#.into()),
            read_at: None,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&n).unwrap();
        // serde rename = "type" so the JSON uses `type` (not `notification_type`).
        assert!(s.contains("\"type\":\"sla_breach\""), "got: {s}");
        assert!(s.contains("\"severity\":\"critical\""));
        assert!(s.contains("\"target_user_id\":42"));
    }
}
