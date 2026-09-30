//! Ticket priority + ticket states (M3-T04).
//!
//! Per spec M3: "priority, ticket states." Per spec: "a local priority and
//! a configurable state layer with per-transition history and lifecycle
//! metrics, layered over Help Scout status, never replacing it."
//!
//! This module provides:
//! - `TicketPriority` enum: urgent / high / normal / low.
//! - `TicketState`: configurable custom states with per-transition history.
//! - Transition recording (who/when/from/to).
//! - Lifecycle metrics (time in each state).
//!
//! Per KNOWN PITFALLS: "Status write and closed_at stamp in one transaction."

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The local priority layer. Per spec: "layered over Help Scout status,
/// never replacing it." Help Scout doesn't have a priority field — this is
/// a SupportOS++-local addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketPriority {
    Urgent,
    High,
    Normal,
    Low,
}

impl TicketPriority {
    pub const ALL: [Self; 4] = [Self::Urgent, Self::High, Self::Normal, Self::Low];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Urgent => "urgent",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "urgent" => Some(Self::Urgent),
            "high" => Some(Self::High),
            "normal" => Some(Self::Normal),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// A ticket state transition record (history entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    pub id: Option<i64>,
    pub conversation_remote_id: i64,
    pub from_state: Option<String>,
    pub to_state: String,
    pub from_priority: Option<String>,
    pub to_priority: Option<String>,
    pub actor_type: String,
    pub actor_id: Option<i64>,
    pub transitioned_at: String,
}

/// The M004 migration: creates ticket_state_transitions table + adds
/// `supportos_priority` column to conversations (per spec: "layered over
/// Help Scout status, never replacing it" — the priority is a separate column).
pub const M004_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS ticket_state_transitions (
        id                     INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_remote_id INTEGER NOT NULL,
        from_state             TEXT,
        to_state               TEXT NOT NULL,
        from_priority          TEXT,
        to_priority            TEXT,
        actor_type             TEXT NOT NULL,
        actor_id               INTEGER,
        transitioned_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_transitions_conv
        ON ticket_state_transitions (conversation_remote_id, transitioned_at);

    -- Add the local priority column to conversations.
    -- This is NOT the Help Scout status — it's a SupportOS++-local addition.
    ALTER TABLE conversations ADD COLUMN supportos_priority TEXT;
    ALTER TABLE conversations ADD COLUMN supportos_state TEXT;
"#;

/// Apply M004 migration. Idempotent.
pub fn apply_m004(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ticket_state_transitions (
            id                     INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_remote_id INTEGER NOT NULL,
            from_state             TEXT,
            to_state               TEXT,
            from_priority          TEXT,
            to_priority            TEXT,
            actor_type             TEXT NOT NULL,
            actor_id               INTEGER,
            transitioned_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_transitions_conv
            ON ticket_state_transitions (conversation_remote_id, transitioned_at);",
    )?;

    for col in ["supportos_priority TEXT", "supportos_state TEXT"] {
        let sql = format!("ALTER TABLE conversations ADD COLUMN {col}");
        if let Err(e) = conn.execute(&sql, []) {
            if !e.to_string().contains("duplicate column name") {
                return Err(crate::error::Error::Sqlite(e));
            }
        }
    }

    Ok(())
}

/// Set the priority on a conversation. Records a transition in the history.
///
/// Per KNOWN PITFALLS: "Status write and closed_at stamp in one transaction."
/// This function updates the `supportos_priority` column and records the
/// transition in `ticket_state_transitions` in a single transaction.
pub fn set_priority(
    conn: &mut Connection,
    conversation_remote_id: i64,
    new_priority: TicketPriority,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<()> {
    let tx = conn.transaction()?;

    // Read the current priority (for the transition record).
    let old_priority: Option<String> = tx
        .query_row(
            "SELECT supportos_priority FROM conversations WHERE remote_id = ?1",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    // Update the priority column.
    tx.execute(
        "UPDATE conversations SET supportos_priority = ?1 WHERE remote_id = ?2",
        params![new_priority.as_str(), conversation_remote_id],
    )?;

    // Record the transition.
    tx.execute(
        "INSERT INTO ticket_state_transitions
            (conversation_remote_id, from_priority, to_priority, actor_type, actor_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            conversation_remote_id,
            old_priority,
            new_priority.as_str(),
            actor_type,
            actor_id,
        ],
    )?;

    tx.commit()?;
    Ok(())
}

/// Set the custom ticket state on a conversation. Records a transition.
pub fn set_ticket_state(
    conn: &mut Connection,
    conversation_remote_id: i64,
    new_state: &str,
    actor_type: &str,
    actor_id: Option<i64>,
) -> Result<()> {
    let tx = conn.transaction()?;

    let old_state: Option<String> = tx
        .query_row(
            "SELECT supportos_state FROM conversations WHERE remote_id = ?1",
            params![conversation_remote_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    tx.execute(
        "UPDATE conversations SET supportos_state = ?1 WHERE remote_id = ?2",
        params![new_state, conversation_remote_id],
    )?;

    tx.execute(
        "INSERT INTO ticket_state_transitions
            (conversation_remote_id, from_state, to_state, actor_type, actor_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            conversation_remote_id,
            old_state,
            new_state,
            actor_type,
            actor_id
        ],
    )?;

    tx.commit()?;
    Ok(())
}

/// Get the priority history for a conversation.
pub fn get_priority_history(
    conn: &Connection,
    conversation_remote_id: i64,
) -> Result<Vec<StateTransition>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_remote_id, from_state, to_state, from_priority, to_priority,
                actor_type, actor_id, transitioned_at
         FROM ticket_state_transitions
         WHERE conversation_remote_id = ?1 AND (from_priority IS NOT NULL OR to_priority IS NOT NULL)
         ORDER BY transitioned_at ASC",
    )?;
    let rows = stmt
        .query_map(params![conversation_remote_id], |r| {
            Ok(StateTransition {
                id: Some(r.get(0)?),
                conversation_remote_id: r.get(1)?,
                from_state: r.get::<_, Option<String>>(2)?,
                to_state: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                from_priority: r.get::<_, Option<String>>(4)?,
                to_priority: r.get::<_, Option<String>>(5)?,
                actor_type: r.get(6)?,
                actor_id: r.get(7)?,
                transitioned_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Get the state history for a conversation.
pub fn get_state_history(
    conn: &Connection,
    conversation_remote_id: i64,
) -> Result<Vec<StateTransition>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_remote_id, from_state, to_state, from_priority, to_priority,
                actor_type, actor_id, transitioned_at
         FROM ticket_state_transitions
         WHERE conversation_remote_id = ?1 AND (from_state IS NOT NULL OR to_state IS NOT NULL)
         ORDER BY transitioned_at ASC",
    )?;
    let rows = stmt
        .query_map(params![conversation_remote_id], |r| {
            Ok(StateTransition {
                id: Some(r.get(0)?),
                conversation_remote_id: r.get(1)?,
                from_state: r.get::<_, Option<String>>(2)?,
                to_state: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                from_priority: r.get::<_, Option<String>>(4)?,
                to_priority: r.get::<_, Option<String>>(5)?,
                actor_type: r.get(6)?,
                actor_id: r.get(7)?,
                transitioned_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Compute lifecycle metrics: time spent in each state (in seconds).
/// Returns a map of state → total_seconds.
pub fn compute_state_durations(
    conn: &Connection,
    conversation_remote_id: i64,
) -> Result<Vec<(String, f64)>> {
    let transitions = get_state_history(conn, conversation_remote_id)?;
    if transitions.is_empty() {
        return Ok(Vec::new());
    }

    let mut durations: Vec<(String, f64)> = Vec::new();
    for i in 0..transitions.len() {
        let from = transitions[i].to_state.as_str();
        let to_time = if i + 1 < transitions.len() {
            transitions[i + 1].transitioned_at.as_str()
        } else {
            // Last transition — measure until now.
            "now"
        };
        let from_time = transitions[i].transitioned_at.as_str();

        let duration_secs: f64 = conn
            .query_row(
                "SELECT (julianday(?1) - julianday(?2)) * 86400.0",
                params![to_time, from_time],
                |r| r.get(0),
            )
            .unwrap_or(0.0);

        durations.push((from.to_string(), duration_secs));
    }
    Ok(durations)
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
        apply_m004(&conn).unwrap();
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
    fn priority_enum_round_trip() {
        for p in TicketPriority::ALL {
            let s = p.as_str();
            let back = TicketPriority::parse(s).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn priority_parse_rejects_unknown() {
        assert!(TicketPriority::parse("unknown").is_none());
        assert!(TicketPriority::parse("").is_none());
    }

    #[test]
    fn set_priority_records_transition() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        set_priority(&mut conn, 1001, TicketPriority::High, "agent", Some(1)).unwrap();

        // The column is updated.
        let priority: Option<String> = conn
            .query_row(
                "SELECT supportos_priority FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(priority, Some("high".into()));

        // A transition was recorded.
        let history = get_priority_history(&conn, 1001).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].to_priority, Some("high".into()));
        assert!(history[0].from_priority.is_none()); // First transition: no old priority.
    }

    #[test]
    fn set_priority_transition_from_old_value() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        set_priority(&mut conn, 1001, TicketPriority::Normal, "agent", Some(1)).unwrap();
        set_priority(&mut conn, 1001, TicketPriority::Urgent, "agent", Some(1)).unwrap();

        let history = get_priority_history(&conn, 1001).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].to_priority, Some("normal".into()));
        assert_eq!(history[1].from_priority, Some("normal".into()));
        assert_eq!(history[1].to_priority, Some("urgent".into()));
    }

    #[test]
    fn set_ticket_state_records_transition() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        set_ticket_state(&mut conn, 1001, "waiting_on_customer", "agent", Some(1)).unwrap();

        let state: Option<String> = conn
            .query_row(
                "SELECT supportos_state FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, Some("waiting_on_customer".into()));

        let history = get_state_history(&conn, 1001).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].to_state, "waiting_on_customer");
    }

    #[test]
    fn state_transitions_are_ordered_chronologically() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        set_ticket_state(&mut conn, 1001, "state_a", "agent", Some(1)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        set_ticket_state(&mut conn, 1001, "state_b", "agent", Some(1)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        set_ticket_state(&mut conn, 1001, "state_c", "agent", Some(1)).unwrap();

        let history = get_state_history(&conn, 1001).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].to_state, "state_a");
        assert_eq!(history[1].to_state, "state_b");
        assert_eq!(history[2].to_state, "state_c");
    }

    #[test]
    fn compute_state_durations_returns_positive_values() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        set_ticket_state(&mut conn, 1001, "state_a", "agent", Some(1)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        set_ticket_state(&mut conn, 1001, "state_b", "agent", Some(1)).unwrap();

        let durations = compute_state_durations(&conn, 1001).unwrap();
        assert_eq!(durations.len(), 2);
        assert_eq!(durations[0].0, "state_a");
        assert!(durations[0].1 > 0.0, "time in state_a should be > 0");
        assert_eq!(durations[1].0, "state_b");
        // state_b is the last → time until now (should also be > 0).
        assert!(durations[1].1 >= 0.0);
    }

    #[test]
    fn priority_and_state_are_independent() {
        let mut conn = fresh_db();
        insert_conversation(&conn, 1001);

        set_priority(&mut conn, 1001, TicketPriority::High, "agent", Some(1)).unwrap();
        set_ticket_state(&mut conn, 1001, "escalated", "agent", Some(1)).unwrap();

        // Both columns are set independently.
        let (priority, state): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT supportos_priority, supportos_state FROM conversations WHERE remote_id = 1001",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(priority, Some("high".into()));
        assert_eq!(state, Some("escalated".into()));

        // Priority history only has priority transitions.
        let ph = get_priority_history(&conn, 1001).unwrap();
        assert_eq!(ph.len(), 1);
        assert!(ph[0].to_priority.is_some());

        // State history only has state transitions.
        let sh = get_state_history(&conn, 1001).unwrap();
        assert_eq!(sh.len(), 1);
        assert_eq!(sh[0].to_state, "escalated");
    }

    #[test]
    fn m004_is_idempotent() {
        let conn = fresh_db();
        apply_m004(&conn).unwrap();
    }
}
