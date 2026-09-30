//! Automation engine — rules + trigger/action + approval queue (M4-T10).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! ## Design
//!
//! - `AutomationRule` — a (trigger, action) pair, with an `enabled` flag.
//!   Stored in the `automation_rules` table as JSON.
//! - When a [`Trigger`] matches an event, the engine evaluates whether the
//!   rule's [`Action`] requires approval. High-impact actions (e.g., assigning
//!   to a different team, changing priority to urgent) require approval;
//!   low-impact actions (e.g., adding an internal note) execute directly.
//! - For high-impact actions, a row is inserted into `automation_approvals`
//!   with `status = 'pending'`. The Operations Center tile
//!   `AutomationApprovals` (M4-T01 stub) is wired here to count
//!   `WHERE status = 'pending'`.
//! - Actions that mutate conversation state go through
//!   `ticket_ops::execute()` (M3-T05 write-protection pipeline — single
//!   source of truth).
//!
//! Per spec: AI is advisory. Automation is deterministic — rules are explicit
//! (trigger + action), not AI-driven.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The M007 migration: creates `automation_rules` + `automation_approvals`
/// tables + indexes.
pub const M007_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS automation_rules (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        name            TEXT NOT NULL,
        trigger_json    TEXT NOT NULL,
        action_json     TEXT NOT NULL,
        enabled         INTEGER NOT NULL DEFAULT 1,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );

    CREATE TABLE IF NOT EXISTS automation_approvals (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        rule_id             INTEGER NOT NULL REFERENCES automation_rules (id) ON DELETE CASCADE,
        conversation_id     INTEGER NOT NULL,
        proposed_action_json TEXT NOT NULL,
        status              TEXT NOT NULL DEFAULT 'pending',
        decided_by_user_id  INTEGER,
        decided_at          TEXT,
        created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_automation_approvals_status
        ON automation_approvals (status, created_at);

    UPDATE app_state SET schema_version = 7 WHERE id = 1;
"#;

/// Apply M007 migration. Idempotent.
pub fn apply_m007(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS automation_rules (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            name            TEXT NOT NULL,
            trigger_json    TEXT NOT NULL,
            action_json     TEXT NOT NULL,
            enabled         INTEGER NOT NULL DEFAULT 1,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );

        CREATE TABLE IF NOT EXISTS automation_approvals (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id             INTEGER NOT NULL REFERENCES automation_rules (id) ON DELETE CASCADE,
            conversation_id     INTEGER NOT NULL,
            proposed_action_json TEXT NOT NULL,
            status              TEXT NOT NULL DEFAULT 'pending',
            decided_by_user_id  INTEGER,
            decided_at          TEXT,
            created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_automation_approvals_status
            ON automation_approvals (status, created_at);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 7 WHERE id = 1", []);
    Ok(())
}

/// A closed vocabulary of triggers. Per spec A12: closed vocabularies are
/// single-source-of-truth enums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// Fires when a conversation's status changes.
    /// `from_status` and `to_status` are optional filters (None = wildcard).
    StatusChanged {
        /// The previous status, or None to match any.
        from_status: Option<String>,
        /// The new status, or None to match any.
        to_status: Option<String>,
    },
    /// Fires when a tag is added to a conversation.
    TagAdded {
        /// The tag name to match.
        tag: String,
    },
    /// Fires when a conversation's priority is set to urgent.
    /// (Future: SLA risk threshold is configurable in M7.)
    SlaRisk,
}

/// A closed vocabulary of actions. Each action corresponds to a
/// `TicketOperation` variant from M3-T05 — actions route through
/// `ticket_ops::execute()` (single source of truth for state mutations).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Assign the conversation to a specific agent. High-impact → requires approval.
    Assign {
        /// The new assignee (Help Scout user remote_id).
        assignee_remote_id: i64,
    },
    /// Add a tag to the conversation. Low-impact → executes directly.
    AddTag {
        /// The tag name to add.
        tag: String,
    },
    /// Send an internal note. Low-impact → executes directly.
    SendNote {
        /// The note body.
        body: String,
    },
    /// Change the conversation's status. High-impact → requires approval.
    ChangeStatus {
        /// The new status.
        new_status: String,
    },
    /// Set the local priority. High-impact when targeting urgent.
    SetPriority {
        /// The new priority.
        new_priority: String,
    },
}

impl Action {
    /// Whether the action requires approval before execution. Per spec:
    /// high-impact actions (assigning, status change, urgent priority)
    /// require human approval; low-impact actions (add tag, send note,
    /// non-urgent priority change) execute directly.
    #[must_use]
    pub fn requires_approval(&self) -> bool {
        match self {
            Self::Assign { .. } | Self::ChangeStatus { .. } => true,
            Self::SetPriority { new_priority } => new_priority == "urgent",
            Self::AddTag { .. } | Self::SendNote { .. } => false,
        }
    }
}

/// An automation rule — a (trigger, action) pair with an `enabled` flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationRule {
    /// The row id (None for new rules).
    pub id: Option<i64>,
    /// The human-readable rule name.
    pub name: String,
    /// The trigger that fires the rule.
    pub trigger: Trigger,
    /// The action to take when the trigger fires.
    pub action: Action,
    /// Whether the rule is enabled (disabled rules never fire).
    pub enabled: bool,
    /// When the rule was created (ISO-8601 UTC).
    pub created_at: Option<String>,
}

/// An automation approval row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationApproval {
    /// The row id.
    pub id: Option<i64>,
    /// The rule that proposed this action.
    pub rule_id: i64,
    /// The conversation the action targets.
    pub conversation_id: i64,
    /// The proposed action (JSON-encoded `Action`).
    pub proposed_action_json: String,
    /// The approval status: 'pending', 'approved', or 'rejected'.
    pub status: String,
    /// The user who decided the approval (None while pending).
    pub decided_by_user_id: Option<i64>,
    /// When the approval was decided (None while pending).
    pub decided_at: Option<String>,
    /// When the approval row was created (ISO-8601 UTC).
    pub created_at: String,
}

/// Create a new automation rule. Returns the new rule's row id.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the insert fails, or `Error::Config` if the
/// trigger/action serialization fails.
pub fn create_rule(conn: &Connection, rule: &AutomationRule) -> Result<i64> {
    let trigger_json = serde_json::to_string(&rule.trigger)
        .map_err(|e| crate::error::Error::Config(format!("trigger serialization failed: {e}")))?;
    let action_json = serde_json::to_string(&rule.action)
        .map_err(|e| crate::error::Error::Config(format!("action serialization failed: {e}")))?;
    conn.execute(
        "INSERT INTO automation_rules (name, trigger_json, action_json, enabled)
         VALUES (?1, ?2, ?3, ?4)",
        params![rule.name, trigger_json, action_json, rule.enabled as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Load an automation rule by id. Returns `None` if not found.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails, or `Error::Config` if the
/// JSON deserialization fails.
pub fn load_rule(conn: &Connection, rule_id: i64) -> Result<Option<AutomationRule>> {
    let row: Option<(i64, String, String, String, i64, Option<String>)> = conn
        .query_row(
            "SELECT id, name, trigger_json, action_json, enabled, created_at
             FROM automation_rules WHERE id = ?1",
            params![rule_id],
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
        .ok();
    match row {
        None => Ok(None),
        Some((id, name, trigger_json, action_json, enabled, created_at)) => {
            let trigger: Trigger = serde_json::from_str(&trigger_json).map_err(|e| {
                crate::error::Error::Config(format!("trigger deserialization failed: {e}"))
            })?;
            let action: Action = serde_json::from_str(&action_json).map_err(|e| {
                crate::error::Error::Config(format!("action deserialization failed: {e}"))
            })?;
            Ok(Some(AutomationRule {
                id: Some(id),
                name,
                trigger,
                action,
                enabled: enabled != 0,
                created_at,
            }))
        }
    }
}

/// List all automation rules (enabled + disabled). Ordered by id ASC.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_rules(conn: &Connection) -> Result<Vec<AutomationRule>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, trigger_json, action_json, enabled, created_at
         FROM automation_rules ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut rules = Vec::new();
    for row in rows {
        let (id, name, trigger_json, action_json, enabled, created_at) = row?;
        let trigger: Trigger = serde_json::from_str(&trigger_json).map_err(|e| {
            crate::error::Error::Config(format!("trigger deserialization failed: {e}"))
        })?;
        let action: Action = serde_json::from_str(&action_json).map_err(|e| {
            crate::error::Error::Config(format!("action deserialization failed: {e}"))
        })?;
        rules.push(AutomationRule {
            id: Some(id),
            name,
            trigger,
            action,
            enabled: enabled != 0,
            created_at,
        });
    }
    Ok(rules)
}

/// Set the `enabled` flag on a rule. Returns `true` if the rule was updated.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn set_rule_enabled(conn: &Connection, rule_id: i64, enabled: bool) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE automation_rules SET enabled = ?1 WHERE id = ?2",
        params![enabled as i64, rule_id],
    )?;
    Ok(rows > 0)
}

/// Delete a rule. Returns `true` if a rule was deleted.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the delete fails.
pub fn delete_rule(conn: &Connection, rule_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "DELETE FROM automation_rules WHERE id = ?1",
        params![rule_id],
    )?;
    Ok(rows > 0)
}

/// Check whether a trigger matches an event. The event is described by
/// its (event_type, from_value, to_value) tuple — for M4-T10 we use a
/// simple match against the trigger's filters.
///
/// # Errors
///
/// Never returns an error currently, but kept as a Result for forward
/// compatibility.
pub fn trigger_matches(trigger: &Trigger, event: &AutomationEvent) -> bool {
    match (trigger, event) {
        (
            Trigger::StatusChanged {
                from_status,
                to_status,
            },
            AutomationEvent::StatusChanged { from, to },
        ) => {
            let from_ok =
                from_status.as_deref().is_none() || from_status.as_deref() == Some(from.as_str());
            let to_ok = to_status.as_deref().is_none() || to_status.as_deref() == Some(to.as_str());
            from_ok && to_ok
        }
        (Trigger::TagAdded { tag }, AutomationEvent::TagAdded { tag: added_tag }) => {
            tag == added_tag
        }
        (Trigger::SlaRisk, AutomationEvent::SlaRisk) => true,
        _ => false,
    }
}

/// An event that may trigger automation rules. Mirrors the [`Trigger`] enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationEvent {
    /// A conversation's status changed.
    StatusChanged {
        /// The previous status.
        from: String,
        /// The new status.
        to: String,
    },
    /// A tag was added.
    TagAdded {
        /// The tag name.
        tag: String,
    },
    /// A conversation entered SLA-risk state.
    SlaRisk,
}

/// The result of evaluating rules against an event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvaluateResult {
    /// The number of rules that matched and executed directly (low-impact).
    pub executed_directly: u32,
    /// The number of rules that matched and required approval (pending).
    pub approval_pending: u32,
    /// The number of rules that didn't match.
    pub no_match: u32,
}

/// Evaluate all enabled rules against an event. For matching rules:
/// - Low-impact actions execute directly (no approval needed).
/// - High-impact actions create a pending approval row.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any DB op fails, or `Error::Config` if JSON
/// serialization fails.
pub fn evaluate_rules(
    conn: &Connection,
    event: &AutomationEvent,
    conversation_id: i64,
) -> Result<EvaluateResult> {
    let mut result = EvaluateResult::default();
    for rule in list_rules(conn)? {
        if !rule.enabled {
            continue;
        }
        if !trigger_matches(&rule.trigger, event) {
            result.no_match += 1;
            continue;
        }
        if rule.action.requires_approval() {
            // Create a pending approval row.
            let action_json = serde_json::to_string(&rule.action).map_err(|e| {
                crate::error::Error::Config(format!("action serialization failed: {e}"))
            })?;
            conn.execute(
                "INSERT INTO automation_approvals (rule_id, conversation_id, proposed_action_json, status)
                 VALUES (?1, ?2, ?3, 'pending')",
                params![rule.id, conversation_id, action_json],
            )?;
            result.approval_pending += 1;
        } else {
            // Low-impact action — would execute via ticket_ops::execute()
            // in the real pipeline. For M4-T10 we mark it as executed
            // (the actual ticket_ops call is wired in the Tauri shell).
            result.executed_directly += 1;
        }
    }
    Ok(result)
}

/// Count pending automation approvals. This is the function the Operations
/// Center `AutomationApprovals` tile (M4-T01 stub) uses — wired here.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn count_pending_approvals(conn: &Connection) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM automation_approvals WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

/// List pending automation approvals (newest-first via `julianday(created_at)`).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_pending_approvals(conn: &Connection) -> Result<Vec<AutomationApproval>> {
    let mut stmt = conn.prepare(
        "SELECT id, rule_id, conversation_id, proposed_action_json, status, decided_by_user_id, decided_at, created_at
         FROM automation_approvals
         WHERE status = 'pending'
         ORDER BY julianday(created_at) DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(AutomationApproval {
            id: Some(r.get(0)?),
            rule_id: r.get(1)?,
            conversation_id: r.get(2)?,
            proposed_action_json: r.get(3)?,
            status: r.get(4)?,
            decided_by_user_id: r.get(5)?,
            decided_at: r.get(6)?,
            created_at: r.get(7)?,
        })
    })?;
    let mut approvals = Vec::new();
    for row in rows {
        approvals.push(row?);
    }
    Ok(approvals)
}

/// Approve a pending approval. Records the decider + decision timestamp.
/// Returns `true` if the approval was pending and is now approved.
///
/// Per spec: actions route through `ticket_ops::execute()` — the caller
/// (Tauri shell) is responsible for executing the proposed action via
/// `ticket_ops::execute()` after calling this function. The approval row
/// records the decision; the action execution is separate.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn approve(conn: &Connection, approval_id: i64, decided_by_user_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE automation_approvals
         SET status = 'approved', decided_by_user_id = ?1,
             decided_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?2 AND status = 'pending'",
        params![decided_by_user_id, approval_id],
    )?;
    Ok(rows > 0)
}

/// Reject a pending approval. Returns `true` if the approval was pending
/// and is now rejected.
///
/// # Errors
///
/// Returns `Error::Sqlite` if the update fails.
pub fn reject(conn: &Connection, approval_id: i64, decided_by_user_id: i64) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE automation_approvals
         SET status = 'rejected', decided_by_user_id = ?1,
             decided_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?2 AND status = 'pending'",
        params![decided_by_user_id, approval_id],
    )?;
    Ok(rows > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::apply_m003;
    use crate::notifications::apply_m005;
    use crate::side_threads::apply_m006;
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
        apply_m007(&conn).unwrap();
        conn
    }

    fn sample_rule(name: &str, trigger: Trigger, action: Action) -> AutomationRule {
        AutomationRule {
            id: None,
            name: name.into(),
            trigger,
            action,
            enabled: true,
            created_at: None,
        }
    }

    // ---- M007 migration -----------------------------------------------------

    #[test]
    fn m007_creates_automation_rules_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM automation_rules", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m007_creates_automation_approvals_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM automation_approvals", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m007_creates_status_index() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_automation_approvals_status'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn m007_is_idempotent() {
        let conn = fresh_db();
        apply_m007(&conn).unwrap();
    }

    // ---- Action::requires_approval -----------------------------------------

    #[test]
    fn assign_action_requires_approval() {
        assert!(Action::Assign {
            assignee_remote_id: 42
        }
        .requires_approval());
    }

    #[test]
    fn change_status_action_requires_approval() {
        assert!(Action::ChangeStatus {
            new_status: "closed".into()
        }
        .requires_approval());
    }

    #[test]
    fn set_priority_urgent_requires_approval() {
        assert!(Action::SetPriority {
            new_priority: "urgent".into()
        }
        .requires_approval());
    }

    #[test]
    fn set_priority_normal_does_not_require_approval() {
        assert!(!Action::SetPriority {
            new_priority: "normal".into()
        }
        .requires_approval());
    }

    #[test]
    fn add_tag_does_not_require_approval() {
        assert!(!Action::AddTag { tag: "vip".into() }.requires_approval());
    }

    #[test]
    fn send_note_does_not_require_approval() {
        assert!(!Action::SendNote {
            body: "heads up".into()
        }
        .requires_approval());
    }

    // ---- CRUD: create/load/list ---------------------------------------------

    #[test]
    fn create_rule_returns_row_id() {
        let conn = fresh_db();
        let rule = sample_rule(
            "Close on resolved",
            Trigger::StatusChanged {
                from_status: Some("active".into()),
                to_status: Some("closed".into()),
            },
            Action::SendNote {
                body: "Auto-closed".into(),
            },
        );
        let id = create_rule(&conn, &rule).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn load_rule_returns_the_rule() {
        let conn = fresh_db();
        let rule = sample_rule(
            "Tag VIP",
            Trigger::TagAdded { tag: "vip".into() },
            Action::SendNote {
                body: "VIP customer".into(),
            },
        );
        let id = create_rule(&conn, &rule).unwrap();
        let loaded = load_rule(&conn, id).unwrap().expect("rule should exist");
        assert_eq!(loaded.id, Some(id));
        assert_eq!(loaded.name, "Tag VIP");
        assert!(loaded.enabled);
        assert_eq!(loaded.trigger, Trigger::TagAdded { tag: "vip".into() });
    }

    #[test]
    fn load_rule_returns_none_for_nonexistent() {
        let conn = fresh_db();
        let loaded = load_rule(&conn, 9999).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn list_rules_returns_all_in_id_order() {
        let conn = fresh_db();
        let id1 = create_rule(
            &conn,
            &sample_rule(
                "Rule 1",
                Trigger::SlaRisk,
                Action::SendNote {
                    body: "sla risk".into(),
                },
            ),
        )
        .unwrap();
        let id2 = create_rule(
            &conn,
            &sample_rule(
                "Rule 2",
                Trigger::SlaRisk,
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        let rules = list_rules(&conn).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id, Some(id1));
        assert_eq!(rules[1].id, Some(id2));
    }

    #[test]
    fn list_rules_returns_empty_when_no_rules() {
        let conn = fresh_db();
        let rules = list_rules(&conn).unwrap();
        assert!(rules.is_empty());
    }

    // ---- CRUD: enable/disable/delete ---------------------------------------

    #[test]
    fn set_rule_enabled_toggles_flag() {
        let conn = fresh_db();
        let id = create_rule(
            &conn,
            &sample_rule(
                "Rule",
                Trigger::SlaRisk,
                Action::SendNote { body: "hi".into() },
            ),
        )
        .unwrap();
        assert!(set_rule_enabled(&conn, id, false).unwrap());
        let loaded = load_rule(&conn, id).unwrap().unwrap();
        assert!(!loaded.enabled);
        assert!(set_rule_enabled(&conn, id, true).unwrap());
        let loaded = load_rule(&conn, id).unwrap().unwrap();
        assert!(loaded.enabled);
    }

    #[test]
    fn set_rule_enabled_returns_false_for_nonexistent() {
        let conn = fresh_db();
        assert!(!set_rule_enabled(&conn, 9999, false).unwrap());
    }

    #[test]
    fn delete_rule_removes_row() {
        let conn = fresh_db();
        let id = create_rule(
            &conn,
            &sample_rule(
                "Rule",
                Trigger::SlaRisk,
                Action::SendNote { body: "hi".into() },
            ),
        )
        .unwrap();
        assert!(delete_rule(&conn, id).unwrap());
        assert!(load_rule(&conn, id).unwrap().is_none());
    }

    #[test]
    fn delete_rule_returns_false_for_nonexistent() {
        let conn = fresh_db();
        assert!(!delete_rule(&conn, 9999).unwrap());
    }

    // ---- trigger_matches ----------------------------------------------------

    #[test]
    fn trigger_matches_status_changed_exact() {
        let t = Trigger::StatusChanged {
            from_status: Some("active".into()),
            to_status: Some("closed".into()),
        };
        let e = AutomationEvent::StatusChanged {
            from: "active".into(),
            to: "closed".into(),
        };
        assert!(trigger_matches(&t, &e));
    }

    #[test]
    fn trigger_matches_status_changed_wildcard_from() {
        let t = Trigger::StatusChanged {
            from_status: None,
            to_status: Some("closed".into()),
        };
        let e = AutomationEvent::StatusChanged {
            from: "pending".into(),
            to: "closed".into(),
        };
        assert!(trigger_matches(&t, &e));
    }

    #[test]
    fn trigger_does_not_match_different_status() {
        let t = Trigger::StatusChanged {
            from_status: Some("active".into()),
            to_status: Some("closed".into()),
        };
        let e = AutomationEvent::StatusChanged {
            from: "active".into(),
            to: "pending".into(),
        };
        assert!(!trigger_matches(&t, &e));
    }

    #[test]
    fn trigger_matches_tag_added_exact() {
        let t = Trigger::TagAdded { tag: "vip".into() };
        let e = AutomationEvent::TagAdded { tag: "vip".into() };
        assert!(trigger_matches(&t, &e));
    }

    #[test]
    fn trigger_does_not_match_different_tag() {
        let t = Trigger::TagAdded { tag: "vip".into() };
        let e = AutomationEvent::TagAdded {
            tag: "escalated".into(),
        };
        assert!(!trigger_matches(&t, &e));
    }

    #[test]
    fn trigger_matches_sla_risk() {
        let t = Trigger::SlaRisk;
        let e = AutomationEvent::SlaRisk;
        assert!(trigger_matches(&t, &e));
    }

    #[test]
    fn trigger_does_not_match_different_event_type() {
        let t = Trigger::SlaRisk;
        let e = AutomationEvent::TagAdded { tag: "vip".into() };
        assert!(!trigger_matches(&t, &e));
    }

    // ---- evaluate_rules -----------------------------------------------------

    #[test]
    fn evaluate_rules_low_impact_executes_directly() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Note on tag",
                Trigger::TagAdded { tag: "vip".into() },
                Action::SendNote {
                    body: "VIP tagged".into(),
                },
            ),
        )
        .unwrap();
        let result = evaluate_rules(
            &conn,
            &AutomationEvent::TagAdded { tag: "vip".into() },
            1001,
        )
        .unwrap();
        assert_eq!(result.executed_directly, 1);
        assert_eq!(result.approval_pending, 0);
        assert_eq!(result.no_match, 0);
    }

    #[test]
    fn evaluate_rules_high_impact_creates_pending_approval() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Assign on close",
                Trigger::StatusChanged {
                    from_status: Some("active".into()),
                    to_status: Some("closed".into()),
                },
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        let result = evaluate_rules(
            &conn,
            &AutomationEvent::StatusChanged {
                from: "active".into(),
                to: "closed".into(),
            },
            1001,
        )
        .unwrap();
        assert_eq!(result.executed_directly, 0);
        assert_eq!(result.approval_pending, 1);
        assert_eq!(result.no_match, 0);

        // The pending approval should exist.
        assert_eq!(count_pending_approvals(&conn).unwrap(), 1);
    }

    #[test]
    fn evaluate_rules_no_match_increments_no_match() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Tag vip",
                Trigger::TagAdded { tag: "vip".into() },
                Action::SendNote { body: "hi".into() },
            ),
        )
        .unwrap();
        let result = evaluate_rules(
            &conn,
            &AutomationEvent::TagAdded {
                tag: "escalated".into(),
            },
            1001,
        )
        .unwrap();
        assert_eq!(result.no_match, 1);
        assert_eq!(result.executed_directly, 0);
        assert_eq!(result.approval_pending, 0);
    }

    #[test]
    fn evaluate_rules_skips_disabled_rules() {
        let conn = fresh_db();
        let rule_id = create_rule(
            &conn,
            &sample_rule(
                "Tag vip",
                Trigger::TagAdded { tag: "vip".into() },
                Action::SendNote { body: "hi".into() },
            ),
        )
        .unwrap();
        set_rule_enabled(&conn, rule_id, false).unwrap();
        let result = evaluate_rules(
            &conn,
            &AutomationEvent::TagAdded { tag: "vip".into() },
            1001,
        )
        .unwrap();
        // Disabled rule doesn't match.
        assert_eq!(result.no_match, 0, "disabled rule is skipped entirely");
        assert_eq!(result.executed_directly, 0);
        assert_eq!(result.approval_pending, 0);
    }

    // ---- count_pending_approvals + list + approve/reject -------------------

    #[test]
    fn count_pending_approvals_returns_zero_initially() {
        let conn = fresh_db();
        assert_eq!(count_pending_approvals(&conn).unwrap(), 0);
    }

    #[test]
    fn count_pending_approvals_counts_correctly() {
        let conn = fresh_db();
        // Create 1 rule that requires approval.
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Assign A",
                Trigger::SlaRisk,
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        // Fire the trigger twice → 2 pending approvals (1 rule × 2 fires).
        evaluate_rules(&conn, &AutomationEvent::SlaRisk, 1001).unwrap();
        evaluate_rules(&conn, &AutomationEvent::SlaRisk, 1002).unwrap();
        assert_eq!(count_pending_approvals(&conn).unwrap(), 2);
    }

    #[test]
    fn list_pending_approvals_returns_only_pending() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Assign",
                Trigger::SlaRisk,
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        evaluate_rules(&conn, &AutomationEvent::SlaRisk, 1001).unwrap();
        let approvals = list_pending_approvals(&conn).unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, "pending");
        assert_eq!(approvals[0].conversation_id, 1001);
    }

    #[test]
    fn approve_marks_approval_approved() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Assign",
                Trigger::SlaRisk,
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        evaluate_rules(&conn, &AutomationEvent::SlaRisk, 1001).unwrap();
        let approvals = list_pending_approvals(&conn).unwrap();
        let approval_id = approvals[0].id.unwrap();
        assert!(approve(&conn, approval_id, 99).unwrap());
        // No longer pending.
        assert_eq!(count_pending_approvals(&conn).unwrap(), 0);
        // Decider recorded.
        let (status, decider): (String, Option<i64>) = conn
            .query_row(
                "SELECT status, decided_by_user_id FROM automation_approvals WHERE id = ?1",
                params![approval_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "approved");
        assert_eq!(decider, Some(99));
    }

    #[test]
    fn reject_marks_approval_rejected() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Assign",
                Trigger::SlaRisk,
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        evaluate_rules(&conn, &AutomationEvent::SlaRisk, 1001).unwrap();
        let approvals = list_pending_approvals(&conn).unwrap();
        let approval_id = approvals[0].id.unwrap();
        assert!(reject(&conn, approval_id, 99).unwrap());
        assert_eq!(count_pending_approvals(&conn).unwrap(), 0);
        let status: String = conn
            .query_row(
                "SELECT status FROM automation_approvals WHERE id = ?1",
                params![approval_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "rejected");
    }

    #[test]
    fn approve_returns_false_for_non_pending() {
        let conn = fresh_db();
        // No approval row exists with id=9999 → returns false.
        assert!(!approve(&conn, 9999, 99).unwrap());
    }

    #[test]
    fn approve_is_idempotent_after_first_call() {
        let conn = fresh_db();
        let _ = create_rule(
            &conn,
            &sample_rule(
                "Assign",
                Trigger::SlaRisk,
                Action::Assign {
                    assignee_remote_id: 42,
                },
            ),
        )
        .unwrap();
        evaluate_rules(&conn, &AutomationEvent::SlaRisk, 1001).unwrap();
        let approvals = list_pending_approvals(&conn).unwrap();
        let approval_id = approvals[0].id.unwrap();
        assert!(approve(&conn, approval_id, 99).unwrap());
        // Second call returns false (no longer pending).
        assert!(!approve(&conn, approval_id, 99).unwrap());
    }

    // ---- serde round-trips --------------------------------------------------

    #[test]
    fn trigger_serializes_with_kind_tag() {
        let t = Trigger::SlaRisk;
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"kind\":\"sla_risk\""), "got: {s}");
    }

    #[test]
    fn action_serializes_with_kind_tag() {
        let a = Action::Assign {
            assignee_remote_id: 42,
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains("\"kind\":\"assign\""), "got: {s}");
        assert!(s.contains("\"assignee_remote_id\":42"));
    }

    #[test]
    fn automation_rule_round_trips() {
        let r = AutomationRule {
            id: Some(1),
            name: "Test rule".into(),
            trigger: Trigger::TagAdded { tag: "vip".into() },
            action: Action::SendNote { body: "hi".into() },
            enabled: true,
            created_at: Some("2026-10-01T10:00:00Z".into()),
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: AutomationRule = serde_json::from_str(&s).unwrap();
        assert_eq!(back.name, "Test rule");
        assert_eq!(back.trigger, r.trigger);
        assert_eq!(back.action, r.action);
    }
}
