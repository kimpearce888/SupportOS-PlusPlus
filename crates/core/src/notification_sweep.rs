//! Notification sweep engine — M4-T05.
//!
//! Per CHANGELOG v1.7.x bug fix: "Notification sweep fired too early on
//! first run, spamming the user. Fix: cursors must not init until the first
//! sync settles."
//!
//! ## The guardrail
//!
//! The sweep does NOT initialize its cursor until the first sync has settled
//! (`sync_runs` has at least one row with `status != 'running'`). Before
//! that, [`sweep_once`] is a no-op and returns [`SkippedReason::FirstSyncNotSettled`].
//!
//! Once the first sync settles, the cursor is initialized to the current
//! max `activity_events.id`. This means historical events (already seen
//! by the user during sync) do NOT trigger notifications — only NEW events
//! (after the cursor init) do. This is the structural fix for the v1.7.x bug.
//!
//! ## Trigger mapping
//!
//! The sweep reads `activity_events` and maps each event to a notification
//! type (if any). The current mapping is intentionally minimal (M4 scope):
//! - `event_type = 'message'` AND `actor_type = 'customer'` →
//!   `NotificationType::CustomerReplied` (target = conversation's assignee)
//! - `event_type = 'assignment_change'` →
//!   `NotificationType::TicketAssigned` (target = new assignee)
//!
//! Other notification types (SLA, AI escalation, known-issue, etc.) come
//! from later milestones (M6/M7) and are emitted by their own modules,
//! not by this sweep.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::NotificationType;
use crate::error::Result;
use crate::notifications::record_notification;
use crate::settings;

/// The settings key for the sweep cursor (the max `activity_events.id`
/// the sweep has processed). Stored via the typed settings store.
pub const SWEEP_CURSOR_KEY: &str = "notifications.sweep.last_processed_event_id";

/// The settings key for the "first sync settled" flag. Stored as a bool
/// so the guardrail check is cheap (one row read).
pub const FIRST_SYNC_SETTLED_KEY: &str = "notifications.sweep.first_sync_settled";

/// Why a sweep call was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkippedReason {
    /// The first sync has not settled yet. Per v1.7.x bug fix: the sweep
    /// must not init its cursor until the first sync settles.
    FirstSyncNotSettled,
    /// The cursor was just initialized to the current max event id. No
    /// historical events are processed (so the user isn't spammed with
    /// notifications about events they already saw during sync).
    CursorJustInitialized,
    /// No new activity_events since the last sweep.
    NoNewEvents,
}

/// The result of a single sweep call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepResult {
    /// `Some(reason)` if the sweep was skipped; `None` if it ran.
    pub skipped_reason: Option<SkippedReason>,
    /// The number of notifications emitted (0 if skipped).
    pub notifications_emitted: u32,
    /// The cursor value after this sweep (max event id processed).
    /// `None` if the cursor was never initialized (first-sync guardrail).
    pub cursor_after: Option<i64>,
}

/// Check whether the first sync has settled. Per the v1.7.x bug fix:
/// the sweep cursor must not init until this returns `true`.
///
/// A sync is "settled" when `sync_runs` has at least one row with
/// `status != 'running'` (i.e., 'completed' or 'failed'). The sweep
/// doesn't require the sync to have succeeded — only that it has finished.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn first_sync_settled(conn: &Connection) -> Result<bool> {
    // Cached flag: once we've seen a settled sync, we don't need to re-query.
    // The flag is set when the first settled sync is observed.
    if settings::get_bool(conn, FIRST_SYNC_SETTLED_KEY, false)? {
        return Ok(true);
    }
    // No cached flag — query sync_runs.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_runs WHERE status != 'running'",
        [],
        |r| r.get(0),
    )?;
    if count > 0 {
        // Cache the result so future sweeps don't re-query.
        settings::set_bool(conn, FIRST_SYNC_SETTLED_KEY, true)?;
    }
    Ok(count > 0)
}

/// Get the current sweep cursor (max event id processed) or `None` if
/// the cursor has never been initialized.
fn get_cursor(conn: &Connection) -> Result<Option<i64>> {
    let v = settings::get_i64(conn, SWEEP_CURSOR_KEY, -1)?;
    if v < 0 {
        Ok(None)
    } else {
        Ok(Some(v))
    }
}

/// Set the sweep cursor.
fn set_cursor(conn: &Connection, value: i64) -> Result<()> {
    settings::set_i64(conn, SWEEP_CURSOR_KEY, value)
}

/// The current max `activity_events.id`, or `None` if there are no events.
fn max_event_id(conn: &Connection) -> Result<Option<i64>> {
    let id: Option<i64> = conn
        .query_row("SELECT MAX(id) FROM activity_events", [], |r| r.get(0))
        .ok()
        .flatten();
    Ok(id)
}

/// Run one sweep pass. Per the v1.7.x bug fix: the sweep is a no-op
/// until the first sync settles (so the user isn't spammed with
/// notifications about events they already saw during sync).
///
/// ## Behavior
///
/// 1. If `first_sync_settled` is `false` → return `SkippedReason::FirstSyncNotSettled`.
/// 2. If the cursor is `None` (never initialized):
///    - Initialize the cursor to `max_event_id` (so historical events
///      don't trigger notifications).
///    - Return `SkippedReason::CursorJustInitialized`.
/// 3. If the cursor is initialized and there are new events with `id > cursor`:
///    - For each new event, determine the notification type (if any).
///    - Call `record_notification` with the appropriate type + target + conversation.
///    - Advance the cursor to the max event id processed.
/// 4. If there are no new events → return `SkippedReason::NoNewEvents`.
///
/// All steps after the guardrail check happen in a single transaction
/// (atomic: process + advance cursor + commit).
///
/// # Errors
///
/// Returns `Error::Sqlite` if any query fails, or `Error::Other` for
/// unexpected state.
pub fn sweep_once(conn: &mut Connection) -> Result<SweepResult> {
    // 1. First-sync guardrail (v1.7.x bug fix).
    if !first_sync_settled(conn)? {
        return Ok(SweepResult {
            skipped_reason: Some(SkippedReason::FirstSyncNotSettled),
            notifications_emitted: 0,
            cursor_after: get_cursor(conn)?,
        });
    }

    // 2. Initialize the cursor if needed (skip historical events).
    let cursor = match get_cursor(conn)? {
        None => {
            let max_id = max_event_id(conn)?.unwrap_or(0);
            set_cursor(conn, max_id)?;
            return Ok(SweepResult {
                skipped_reason: Some(SkippedReason::CursorJustInitialized),
                notifications_emitted: 0,
                cursor_after: Some(max_id),
            });
        }
        Some(c) => c,
    };

    // 3. Read new events with id > cursor.
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, event_type, actor_type, actor_id
         FROM activity_events
         WHERE id > ?1
         ORDER BY id ASC",
    )?;
    let new_events: Vec<(i64, i64, String, String, Option<i64>)> = stmt
        .query_map(params![cursor], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    if new_events.is_empty() {
        return Ok(SweepResult {
            skipped_reason: Some(SkippedReason::NoNewEvents),
            notifications_emitted: 0,
            cursor_after: Some(cursor),
        });
    }

    // 4. Process events in a transaction: emit notifications + advance cursor.
    let tx = conn.transaction()?;
    let mut emitted = 0u32;
    let mut max_id_seen = cursor;

    for (event_id, conv_id, event_type, actor_type, actor_id) in &new_events {
        max_id_seen = (*event_id).max(max_id_seen);
        if let Some((notif_type, target_user_id)) =
            map_event_to_notification(&tx, conv_id, event_type, actor_type, actor_id)?
        {
            record_notification(
                &tx,
                &notif_type,
                target_user_id,
                Some(*conv_id),
                Some(
                    &serde_json::json!({
                        "event_id": event_id,
                        "event_type": event_type,
                        "actor_type": actor_type,
                        "actor_id": actor_id,
                    })
                    .to_string(),
                ),
            )?;
            emitted += 1;
        }
    }

    // Advance the cursor atomically with the notification inserts.
    settings::set_i64(&tx, SWEEP_CURSOR_KEY, max_id_seen)?;
    tx.commit()?;

    Ok(SweepResult {
        skipped_reason: None,
        notifications_emitted: emitted,
        cursor_after: Some(max_id_seen),
    })
}

/// Map an activity event to a notification type + target user.
/// Returns `None` if the event doesn't map to any notification.
///
/// Current mapping (M4 scope — minimal; other types come from later milestones):
/// - `event_type = 'message'` AND `actor_type = 'customer'` →
///   `NotificationType::CustomerReplied`, target = conversation's assignee.
/// - `event_type = 'assignment_change'` →
///   `NotificationType::TicketAssigned`, target = the new assignee (actor_id).
///
/// Per spec: AI is advisory — never auto-notify on AI events.
fn map_event_to_notification(
    conn: &Connection,
    conversation_id: &i64,
    event_type: &str,
    actor_type: &str,
    actor_id: &Option<i64>,
) -> Result<Option<(NotificationType, Option<i64>)>> {
    match (event_type, actor_type) {
        ("message", "customer") => {
            // Look up the conversation's assignee.
            let assignee: Option<i64> = conn
                .query_row(
                    "SELECT assignee_id FROM conversations WHERE remote_id = ?1",
                    params![conversation_id],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            Ok(Some((NotificationType::CustomerReplied, assignee)))
        }
        ("assignment_change", _) => {
            // The new assignee is the actor_id (whoever performed the change
            // IS the new assignee in this simplified model — real impl would
            // store the new assignee in the event payload, but for M4 we
            // use the actor_id).
            Ok(Some((NotificationType::TicketAssigned, *actor_id)))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{apply_m003, record_event, ActivityEvent};
    use crate::notifications::{apply_m005, count_unread_for_user};
    use crate::ticket_states::apply_m004;
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
        apply_m004(&conn).unwrap();
        apply_m005(&conn).unwrap();
        conn
    }

    fn insert_conversation(conn: &Connection, remote_id: i64, assignee_id: Option<i64>) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, assignee_id)
             VALUES (?1, ?2, 'active', 101, 2001, ?3)",
            params![remote_id, remote_id, assignee_id],
        )
        .unwrap();
    }

    fn insert_event(
        conn: &Connection,
        conv_remote_id: i64,
        event_type: &str,
        actor: &str,
        actor_id: Option<i64>,
        dedup: &str,
    ) {
        record_event(
            conn,
            &ActivityEvent {
                id: None,
                conversation_id: conv_remote_id,
                event_type: event_type.into(),
                actor_type: actor.into(),
                actor_id,
                occurred_at: "2026-01-01T10:00:00Z".into(),
                dedup_key: dedup.into(),
            },
        )
        .unwrap();
    }

    fn mark_sync_settled(conn: &Connection) {
        conn.execute("INSERT INTO sync_runs (status) VALUES ('completed')", [])
            .unwrap();
    }

    // ---- first_sync_settled guardrail ---------------------------------------

    #[test]
    fn first_sync_settled_returns_false_when_no_sync_runs() {
        let conn = fresh_db();
        assert!(!first_sync_settled(&conn).unwrap());
    }

    #[test]
    fn first_sync_settled_returns_false_when_sync_still_running() {
        let conn = fresh_db();
        conn.execute("INSERT INTO sync_runs (status) VALUES ('running')", [])
            .unwrap();
        assert!(!first_sync_settled(&conn).unwrap());
    }

    #[test]
    fn first_sync_settled_returns_true_when_sync_completed() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        assert!(first_sync_settled(&conn).unwrap());
    }

    #[test]
    fn first_sync_settled_caches_result() {
        let conn = fresh_db();
        mark_sync_settled(&conn);
        // First call queries sync_runs + caches the flag.
        assert!(first_sync_settled(&conn).unwrap());
        // Delete the sync_runs row — the cached flag should still return true.
        conn.execute("DELETE FROM sync_runs", []).unwrap();
        assert!(
            first_sync_settled(&conn).unwrap(),
            "cached flag should return true even after sync_runs is cleared"
        );
    }

    // ---- sweep_once guardrail behavior -------------------------------------

    #[test]
    fn sweep_once_is_noop_before_first_sync_settles() {
        let mut conn = fresh_db();
        // No sync_runs yet → guardrail kicks in.
        let result = sweep_once(&mut conn).unwrap();
        assert_eq!(
            result.skipped_reason,
            Some(SkippedReason::FirstSyncNotSettled)
        );
        assert_eq!(result.notifications_emitted, 0);
        assert!(
            result.cursor_after.is_none(),
            "cursor must NOT be initialized"
        );
    }

    #[test]
    fn sweep_once_initializes_cursor_after_first_sync_settles() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, None);
        // Some historical events (before the cursor init).
        insert_event(&conn, 1001, "message", "customer", None, "evt_001");
        insert_event(&conn, 1001, "message", "customer", None, "evt_002");

        mark_sync_settled(&conn);

        let result = sweep_once(&mut conn).unwrap();
        // Cursor just initialized — no notifications emitted for historical events.
        assert_eq!(
            result.skipped_reason,
            Some(SkippedReason::CursorJustInitialized)
        );
        assert_eq!(
            result.notifications_emitted, 0,
            "historical events must NOT trigger notifications"
        );
        assert!(result.cursor_after.is_some());
        let cursor = result.cursor_after.unwrap();
        assert!(cursor > 0);
    }

    #[test]
    fn sweep_once_emits_notifications_for_new_events_only() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, Some(42));
        // Historical events (before cursor init) — must NOT trigger.
        insert_event(&conn, 1001, "message", "customer", None, "evt_001");

        mark_sync_settled(&conn);

        // First sweep: initializes cursor, no notifications.
        let r1 = sweep_once(&mut conn).unwrap();
        assert_eq!(
            r1.skipped_reason,
            Some(SkippedReason::CursorJustInitialized)
        );

        // New event AFTER cursor init — must trigger a notification.
        insert_event(&conn, 1001, "message", "customer", None, "evt_002");
        let r2 = sweep_once(&mut conn).unwrap();
        assert_eq!(r2.skipped_reason, None, "sweep should have run");
        assert_eq!(r2.notifications_emitted, 1);
        assert!(r2.cursor_after.unwrap() > r1.cursor_after.unwrap());

        // Verify the notification was actually recorded for the assignee (user 42).
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(
            count, 1,
            "CustomerReplied notification should be unread for user 42"
        );
    }

    #[test]
    fn sweep_once_returns_no_new_events_when_idle() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, None);
        mark_sync_settled(&conn);

        // First sweep: initialize cursor.
        let r1 = sweep_once(&mut conn).unwrap();
        assert_eq!(
            r1.skipped_reason,
            Some(SkippedReason::CursorJustInitialized)
        );

        // Second sweep with no new events → NoNewEvents.
        let r2 = sweep_once(&mut conn).unwrap();
        assert_eq!(r2.skipped_reason, Some(SkippedReason::NoNewEvents));
        assert_eq!(r2.notifications_emitted, 0);
        assert_eq!(r2.cursor_after, r1.cursor_after, "cursor should not move");
    }

    #[test]
    fn sweep_once_is_idempotent() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, Some(42));
        mark_sync_settled(&conn);

        // Init cursor.
        sweep_once(&mut conn).unwrap();

        // Add a new event.
        insert_event(&conn, 1001, "message", "customer", None, "evt_001");

        // First sweep after the event → emits 1 notification.
        let r1 = sweep_once(&mut conn).unwrap();
        assert_eq!(r1.notifications_emitted, 1);

        // Second sweep with no new events → 0 notifications (idempotent).
        let r2 = sweep_once(&mut conn).unwrap();
        assert_eq!(r2.notifications_emitted, 0);
        assert_eq!(r2.skipped_reason, Some(SkippedReason::NoNewEvents));

        // Total notifications: still 1.
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 1);
    }

    // ---- event mapping ------------------------------------------------------

    #[test]
    fn customer_message_event_emits_customer_replied_for_assignee() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, Some(42));
        mark_sync_settled(&conn);
        sweep_once(&mut conn).unwrap(); // init cursor

        insert_event(&conn, 1001, "message", "customer", None, "evt_001");
        sweep_once(&mut conn).unwrap();

        // The CustomerReplied notification should be visible to user 42.
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn customer_message_with_no_assignee_emits_broadcast() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, None);
        mark_sync_settled(&conn);
        sweep_once(&mut conn).unwrap(); // init cursor

        insert_event(&conn, 1001, "message", "customer", None, "evt_001");
        sweep_once(&mut conn).unwrap();

        // The CustomerReplied notification is broadcast (no assignee).
        // Verify it's visible to a random user (broadcast target).
        let count = count_unread_for_user(&conn, 999).unwrap();
        assert_eq!(count, 1, "broadcast notifications are visible to all users");
    }

    #[test]
    fn assignment_change_event_emits_ticket_assigned() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, None);
        mark_sync_settled(&conn);
        sweep_once(&mut conn).unwrap(); // init cursor

        // An assignment_change event with actor_id = 42 (the new assignee).
        insert_event(
            &conn,
            1001,
            "assignment_change",
            "agent",
            Some(42),
            "evt_001",
        );
        let r = sweep_once(&mut conn).unwrap();
        assert_eq!(r.notifications_emitted, 1);

        // The TicketAssigned notification is targeted at the new assignee (42).
        let count = count_unread_for_user(&conn, 42).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn agent_message_event_does_not_emit_notification() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, Some(42));
        mark_sync_settled(&conn);
        sweep_once(&mut conn).unwrap(); // init cursor

        // An agent reply event — should NOT trigger a notification (only
        // customer messages do, per the minimal M4 mapping).
        insert_event(&conn, 1001, "message", "agent", Some(43), "evt_001");
        let r = sweep_once(&mut conn).unwrap();
        assert_eq!(r.notifications_emitted, 0);
    }

    #[test]
    fn unknown_event_type_does_not_emit_notification() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, Some(42));
        mark_sync_settled(&conn);
        sweep_once(&mut conn).unwrap(); // init cursor

        insert_event(&conn, 1001, "unknown_event_type", "system", None, "evt_001");
        let r = sweep_once(&mut conn).unwrap();
        assert_eq!(r.notifications_emitted, 0);
    }

    // ---- cursor advancement -------------------------------------------------

    #[test]
    fn cursor_advances_to_max_event_id_processed() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001, Some(42));
        mark_sync_settled(&conn);
        sweep_once(&mut conn).unwrap(); // init cursor

        // Add 3 events.
        insert_event(&conn, 1001, "message", "customer", None, "evt_001");
        insert_event(&conn, 1001, "message", "customer", None, "evt_002");
        insert_event(&conn, 1001, "message", "customer", None, "evt_003");

        let r = sweep_once(&mut conn).unwrap();
        assert_eq!(r.notifications_emitted, 3);

        // The cursor should be at the max event id.
        let max_id: i64 = conn
            .query_row("SELECT MAX(id) FROM activity_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(r.cursor_after.unwrap(), max_id);

        // Re-sweep → no new events.
        let r2 = sweep_once(&mut conn).unwrap();
        assert_eq!(r2.skipped_reason, Some(SkippedReason::NoNewEvents));
    }

    #[test]
    fn sweep_result_serializes_with_kind_tag() {
        let r = SweepResult {
            skipped_reason: Some(SkippedReason::FirstSyncNotSettled),
            notifications_emitted: 0,
            cursor_after: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            s.contains("\"skipped_reason\":\"first_sync_not_settled\""),
            "got: {s}"
        );
        assert!(s.contains("\"notifications_emitted\":0"));

        let r2 = SweepResult {
            skipped_reason: None,
            notifications_emitted: 3,
            cursor_after: Some(42),
        };
        let s2 = serde_json::to_string(&r2).unwrap();
        assert!(s2.contains("\"skipped_reason\":null"), "got: {s2}");
        assert!(s2.contains("\"cursor_after\":42"));
    }
}
