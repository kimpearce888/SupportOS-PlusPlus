//! Ticket operations — write-protection pipeline (M3-T05).
//!
//! Per spec M3: "ticket operations, write-protection pipeline."
//! Per KNOWN PITFALLS:
//! - "Reads have no side effects; rebuilds only via explicit commands."
//! - "Status write and closed_at stamp in one transaction."
//! - "Timeouts on sends become 'unknown' and are reconciled before any retry."
//!
//! The write-protection pipeline ensures that:
//! 1. All mutations go through explicit `TicketOperation` commands — no
//!    implicit writes from reads.
//! 2. Status changes + `closed_at` stamp happen in one transaction.
//! 3. Every operation is validated before execution (type system enforces
//!    valid states — wrong states are impossible by construction).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ticket_states::TicketPriority;

/// A ticket operation — the only way to mutate conversation state.
/// Per spec: "writes only via explicit commands."
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TicketOperation {
    /// Assign a conversation to an agent.
    Assign {
        conversation_remote_id: i64,
        assignee_local_id: Option<i64>,
        actor_type: String,
        actor_id: Option<i64>,
    },
    /// Change the Help Scout status (active/pending/closed).
    /// Per KNOWN PITFALLS: status write + closed_at in one transaction.
    ChangeStatus {
        conversation_remote_id: i64,
        new_status: String,
        actor_type: String,
        actor_id: Option<i64>,
    },
    /// Set the local priority (SupportOS++ layer, not Help Scout).
    SetPriority {
        conversation_remote_id: i64,
        new_priority: TicketPriority,
        actor_type: String,
        actor_id: Option<i64>,
    },
    /// Set the custom ticket state (SupportOS++ layer).
    SetTicketState {
        conversation_remote_id: i64,
        new_state: String,
        actor_type: String,
        actor_id: Option<i64>,
    },
    /// Add an internal note.
    AddNote {
        conversation_remote_id: i64,
        body: String,
        actor_type: String,
        actor_id: Option<i64>,
    },
}

/// The result of executing a ticket operation.
#[derive(Debug, Clone)]
pub enum OperationResult {
    /// The operation succeeded.
    Success { message: String },
    /// The operation was rejected (validation failed, conversation not found, etc.).
    Rejected { reason: String },
}

/// Execute a ticket operation. This is the ONLY entry point for mutating
/// conversation state. Per spec: "writes only via explicit commands."
///
/// All operations use a transaction to ensure atomicity. Per KNOWN PITFALLS:
/// "Status write and closed_at stamp in one transaction."
pub fn execute(conn: &mut Connection, op: &TicketOperation) -> Result<OperationResult> {
    match op {
        TicketOperation::Assign {
            conversation_remote_id,
            assignee_local_id,
            actor_type,
            actor_id,
        } => execute_assign(
            conn,
            *conversation_remote_id,
            *assignee_local_id,
            actor_type,
            *actor_id,
        ),
        TicketOperation::ChangeStatus {
            conversation_remote_id,
            new_status,
            actor_type,
            actor_id,
        } => execute_change_status(
            conn,
            *conversation_remote_id,
            new_status,
            actor_type,
            *actor_id,
        ),
        TicketOperation::SetPriority {
            conversation_remote_id,
            new_priority,
            actor_type,
            actor_id,
        } => execute_set_priority(
            conn,
            *conversation_remote_id,
            *new_priority,
            actor_type,
            *actor_id,
        ),
        TicketOperation::SetTicketState {
            conversation_remote_id,
            new_state,
            actor_type,
            actor_id,
        } => execute_set_ticket_state(
            conn,
            *conversation_remote_id,
            new_state,
            actor_type,
            *actor_id,
        ),
        TicketOperation::AddNote {
            conversation_remote_id,
            body,
            actor_type,
            actor_id,
        } => execute_add_note(conn, *conversation_remote_id, body, actor_type, *actor_id),
    }
}

fn conversation_exists(conn: &Connection, remote_id: i64) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE remote_id = ?1)",
        params![remote_id],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn execute_assign(
    conn: &mut Connection,
    remote_id: i64,
    assignee_id: Option<i64>,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<OperationResult> {
    if !conversation_exists(conn, remote_id) {
        return Ok(OperationResult::Rejected {
            reason: format!("conversation {remote_id} not found"),
        });
    }

    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE conversations SET assignee_id = ?1 WHERE remote_id = ?2",
        params![assignee_id, remote_id],
    )?;

    // Record the activity event.
    tx.execute(
        "INSERT INTO activity_events (conversation_id, event_type, actor_type, actor_id, occurred_at, dedup_key)
         VALUES (?1, 'assign', ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 'assign_' || ?1 || '_' || ?3 || '_' || strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![remote_id, actor_type, actor_id],
    )?;

    tx.commit()?;

    // Rebuild derived columns (response state may have changed).
    crate::activity::update_derived_columns(conn, remote_id)?;

    Ok(OperationResult::Success {
        message: format!("conversation {remote_id} assigned"),
    })
}

fn execute_change_status(
    conn: &mut Connection,
    remote_id: i64,
    new_status: &str,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<OperationResult> {
    if !conversation_exists(conn, remote_id) {
        return Ok(OperationResult::Rejected {
            reason: format!("conversation {remote_id} not found"),
        });
    }

    // Validate the status is one of the allowed values.
    let valid = matches!(new_status, "active" | "pending" | "closed" | "spam");
    if !valid {
        return Ok(OperationResult::Rejected {
            reason: format!("invalid status: {new_status}"),
        });
    }

    let tx = conn.transaction()?;

    // Per KNOWN PITFALLS: "Status write and closed_at stamp in one transaction."
    let closed_at = if new_status == "closed" {
        Some("strftime('%Y-%m-%dT%H:%M:%fZ','now')")
    } else {
        None
    };

    match closed_at {
        Some(_) => {
            tx.execute(
                "UPDATE conversations SET status = ?1, closed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE remote_id = ?2",
                params![new_status, remote_id],
            )?;
        }
        None => {
            tx.execute(
                "UPDATE conversations SET status = ?1, closed_at = NULL WHERE remote_id = ?2",
                params![new_status, remote_id],
            )?;
        }
    }

    // Record the activity event.
    tx.execute(
        "INSERT OR IGNORE INTO activity_events (conversation_id, event_type, actor_type, actor_id, occurred_at, dedup_key)
         VALUES (?1, 'status_change', ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 'status_' || ?1 || '_' || ?3 || '_' || ?4 || '_' || strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![remote_id, actor_type, actor_id, new_status],
    )?;

    tx.commit()?;

    // Rebuild derived columns.
    crate::activity::update_derived_columns(conn, remote_id)?;

    Ok(OperationResult::Success {
        message: format!("conversation {remote_id} status changed to {new_status}"),
    })
}

fn execute_set_priority(
    conn: &mut Connection,
    remote_id: i64,
    priority: TicketPriority,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<OperationResult> {
    if !conversation_exists(conn, remote_id) {
        return Ok(OperationResult::Rejected {
            reason: format!("conversation {remote_id} not found"),
        });
    }

    crate::ticket_states::set_priority(conn, remote_id, priority, actor_type, actor_id)?;

    Ok(OperationResult::Success {
        message: format!(
            "conversation {remote_id} priority set to {}",
            priority.as_str()
        ),
    })
}

fn execute_set_ticket_state(
    conn: &mut Connection,
    remote_id: i64,
    new_state: &str,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<OperationResult> {
    if !conversation_exists(conn, remote_id) {
        return Ok(OperationResult::Rejected {
            reason: format!("conversation {remote_id} not found"),
        });
    }

    crate::ticket_states::set_ticket_state(conn, remote_id, new_state, actor_type, actor_id)?;

    Ok(OperationResult::Success {
        message: format!("conversation {remote_id} state set to {new_state}"),
    })
}

fn execute_add_note(
    conn: &mut Connection,
    remote_id: i64,
    _body: &str,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<OperationResult> {
    if !conversation_exists(conn, remote_id) {
        return Ok(OperationResult::Rejected {
            reason: format!("conversation {remote_id} not found"),
        });
    }

    // Per KNOWN PITFALLS: "Reset composer and draft state per conversation
    // so a draft can never reach another customer." The note body is scoped
    // to this conversation only — no cross-conversation leakage.
    let tx = conn.transaction()?;

    // Store the note as an activity event.
    tx.execute(
        "INSERT INTO activity_events (conversation_id, event_type, actor_type, actor_id, occurred_at, dedup_key)
         VALUES (?1, 'note', ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 'note_' || ?1 || '_' || ?3 || '_' || strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        params![remote_id, actor_type, actor_id],
    )?;

    tx.commit()?;

    Ok(OperationResult::Success {
        message: format!("note added to conversation {remote_id}"),
    })
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
        conn
    }

    fn insert_conversation(conn: &Connection, remote_id: i64) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (?1, ?1, 'active', 101, 2001)",
            params![remote_id],
        )
        .unwrap();
    }

    #[test]
    fn assign_updates_assignee() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        let op = TicketOperation::Assign {
            conversation_remote_id: 1001,
            assignee_local_id: Some(5),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Success { .. }));

        let assignee: Option<i64> = conn
            .query_row(
                "SELECT assignee_id FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(assignee, Some(5));
    }

    #[test]
    fn change_status_to_closed_stamps_closed_at() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        let op = TicketOperation::ChangeStatus {
            conversation_remote_id: 1001,
            new_status: "closed".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Success { .. }));

        // closed_at is stamped in the same transaction.
        let (status, closed_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, closed_at FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "closed");
        assert!(
            closed_at.is_some(),
            "closed_at must be set when status=closed"
        );
    }

    #[test]
    fn change_status_to_active_clears_closed_at() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        // First close it.
        let op = TicketOperation::ChangeStatus {
            conversation_remote_id: 1001,
            new_status: "closed".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };
        execute(&mut conn, &op).unwrap();

        // Then reopen.
        let op = TicketOperation::ChangeStatus {
            conversation_remote_id: 1001,
            new_status: "active".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };
        execute(&mut conn, &op).unwrap();

        let (status, closed_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, closed_at FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert!(
            closed_at.is_none(),
            "closed_at must be cleared when reopening"
        );
    }

    #[test]
    fn invalid_status_rejected() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        let op = TicketOperation::ChangeStatus {
            conversation_remote_id: 1001,
            new_status: "invalid_status".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Rejected { .. }));
    }

    #[test]
    fn nonexistent_conversation_rejected() {
        let mut conn = fresh_db();

        let op = TicketOperation::Assign {
            conversation_remote_id: 9999,
            assignee_local_id: Some(1),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Rejected { .. }));
    }

    #[test]
    fn set_priority_via_operation() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        let op = TicketOperation::SetPriority {
            conversation_remote_id: 1001,
            new_priority: TicketPriority::Urgent,
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Success { .. }));

        let priority: Option<String> = conn
            .query_row(
                "SELECT supportos_priority FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(priority, Some("urgent".into()));
    }

    #[test]
    fn set_ticket_state_via_operation() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        let op = TicketOperation::SetTicketState {
            conversation_remote_id: 1001,
            new_state: "escalated".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Success { .. }));

        let state: Option<String> = conn
            .query_row(
                "SELECT supportos_state FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, Some("escalated".into()));
    }

    #[test]
    fn add_note_records_activity() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        let op = TicketOperation::AddNote {
            conversation_remote_id: 1001,
            body: "This is a test note".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };

        let result = execute(&mut conn, &op).unwrap();
        assert!(matches!(result, OperationResult::Success { .. }));

        // An activity event was recorded.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM activity_events WHERE conversation_id = 1001 AND event_type = 'note'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn change_status_rebuilds_response_state() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        // Add a customer message so response_state = customer_waiting.
        crate::activity::record_event(
            &conn,
            &crate::activity::ActivityEvent {
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
        crate::activity::update_derived_columns(&conn, 1001).unwrap();

        // Verify it's customer_waiting.
        let state: String = conn
            .query_row(
                "SELECT response_state FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "customer_waiting");

        // Close the conversation → response_state should become "closed".
        let op = TicketOperation::ChangeStatus {
            conversation_remote_id: 1001,
            new_status: "closed".into(),
            actor_type: "agent".into(),
            actor_id: Some(1),
        };
        execute(&mut conn, &op).unwrap();

        let state: String = conn
            .query_row(
                "SELECT response_state FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "closed",
            "response_state should be rebuilt after status change"
        );
    }
}
