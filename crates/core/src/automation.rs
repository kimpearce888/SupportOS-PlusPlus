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

// ─── Manual trigger (reference engine.fireTrigger + routes/automation.ts) ──

/// Execute one rule action against a conversation through the
/// write-protection pipeline (`ticket_ops::execute`, the single source of
/// truth for state mutations). Returns the run outcome:
/// `completed` / `failed` / `awaiting_approval`.
///
/// High-impact actions never execute directly — they park a pending
/// `automation_approvals` row (the port's approval tier, mirroring the
/// reference's higher-risk handling).
fn execute_action(
    conn: &mut Connection,
    rule_id: i64,
    action: &Action,
    conversation_remote_id: i64,
    conversation_local_id: i64,
) -> &'static str {
    use crate::ticket_ops::{execute, OperationResult, TicketOperation};
    if action.requires_approval() {
        if let Ok(action_json) = serde_json::to_string(action) {
            let _ = conn.execute(
                "INSERT INTO automation_approvals (rule_id, conversation_id, proposed_action_json, status)
                 VALUES (?1, ?2, ?3, 'pending')",
                params![rule_id, conversation_remote_id, action_json],
            );
        }
        return "awaiting_approval";
    }
    let op = match action {
        Action::Assign { assignee_remote_id } => {
            // The action stores the Help Scout user remote id; the write
            // pipeline wants the local user id.
            let assignee_local_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM users WHERE remote_id = ?1",
                    params![assignee_remote_id],
                    |r| r.get(0),
                )
                .ok();
            TicketOperation::Assign {
                conversation_remote_id,
                assignee_local_id,
                actor_type: "automation".to_string(),
                actor_id: None,
            }
        }
        Action::ChangeStatus { new_status } => TicketOperation::ChangeStatus {
            conversation_remote_id,
            new_status: new_status.clone(),
            actor_type: "automation".to_string(),
            actor_id: None,
        },
        Action::SetPriority { new_priority } => {
            match crate::ticket_states::TicketPriority::parse(new_priority) {
                Some(p) => TicketOperation::SetPriority {
                    conversation_remote_id,
                    new_priority: p,
                    actor_type: "automation".to_string(),
                    actor_id: None,
                },
                None => return "failed", // unknown priority vocabulary
            }
        }
        Action::SendNote { body } => TicketOperation::AddNote {
            conversation_remote_id,
            body: body.clone(),
            actor_type: "automation".to_string(),
            actor_id: None,
        },
        Action::AddTag { tag } => {
            // Tags are not a TicketOperation — they go through the M030
            // conversation_tags join (read current, merge, write back).
            let mut tags =
                crate::conversation_ops::read_conversation_tags(conn, conversation_local_id);
            if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                tags.push(tag.clone());
                crate::conversation_ops::write_conversation_tags(
                    conn,
                    conversation_local_id,
                    &tags,
                );
            }
            return "completed";
        }
    };
    match execute(conn, &op) {
        Ok(OperationResult::Success { .. }) => "completed",
        Ok(OperationResult::Rejected { .. }) => "failed",
        Err(_) => "failed",
    }
}

/// Fire a trigger for a conversation (reference AutomationEngine.fireTrigger,
/// the manual-trigger path from routes/automation.ts:82-88):
///
/// - `automation_enabled` OFF ⇒ no runs.
/// - unknown conversation ⇒ no runs.
/// - every ENABLED rule with the fired trigger runs: high-impact actions
///   park a pending approval (`awaiting_approval`), low-impact actions
///   execute directly through `ticket_ops` (`completed` / `failed`).
/// - each fired rule records one `automation_runs` row; the LAST run row
///   per rule is returned (the reference pushes the last recorded run).
///
/// The conversation id is accepted as either the local or the remote id
/// (the port's routes historically use both); the row's remote id drives
/// the write pipeline.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any DB op fails.
pub fn fire_trigger(
    conn: &mut Connection,
    fired: &Trigger,
    conversation_id: i64,
) -> Result<Vec<serde_json::Value>> {
    if !crate::settings::get_bool(conn, "automation_enabled", false)? {
        return Ok(Vec::new());
    }
    let conversation: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, remote_id FROM conversations WHERE id = ?1 OR remote_id = ?1",
            params![conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((conversation_local_id, conversation_remote_id)) = conversation else {
        return Ok(Vec::new());
    };
    // The M035 automation_runs table (created by intelligence_features).
    crate::intelligence_features::apply_m035(conn)?;
    let mut runs = Vec::new();
    for rule in list_rules(conn)? {
        if !rule.enabled || &rule.trigger != fired {
            continue;
        }
        let rule_id = rule.id.unwrap_or_default();
        let outcome = execute_action(
            conn,
            rule_id,
            &rule.action,
            conversation_remote_id,
            conversation_local_id,
        );
        conn.execute(
            "INSERT INTO automation_runs (rule_id, conversation_id, triggered_by, outcome)
             VALUES (?1, ?2, 'manual', ?3)",
            params![rule_id, conversation_id, outcome],
        )?;
        // The reference returns the LAST recorded run row per rule.
        let last = conn.last_insert_rowid();
        if let Ok(v) = conn.query_row(
            "SELECT id, rule_id, conversation_id, triggered_by, outcome, created_at
             FROM automation_runs WHERE id = ?1",
            params![last],
            |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "rule_id": r.get::<_, i64>(1)?,
                    "conversation_id": r.get::<_, Option<i64>>(2)?,
                    "triggered_by": r.get::<_, String>(3)?,
                    "outcome": r.get::<_, String>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                }))
            },
        ) {
            runs.push(v);
        }
    }
    Ok(runs)
}

// ─────────────────────────────────────────────────────────────────────────────
// MAIN rule model (audit B4 / plan item AU-01).
//
// The reference stores automation rules with a MAIN trigger enum string, a
// conditions JSON array, an actions JSON array, priority and
// requires_approval (migration 003 + engine.ts:34-104), and validates the
// whole surface with `automationRuleSchema` (shared/schemas.ts:499-545). The
// port's M007 table keeps its own column names (trigger_json / action_json —
// audit DB-04) and lacked conditions / priority / requires_approval /
// last_run_at / run_count entirely, and the create route INSERT named
// nonexistent `trigger` / `action` columns — silently swallowed, so rule
// creation never persisted anything (B4).
//
// This section adds the missing columns (guarded ALTERs — the established
// bootstrap pattern), a zod-equivalent validator, MAIN-shaped storage
// functions and the reference fire semantics for the manual-trigger route.
// The old Trigger/Action-enum model above stays untouched (dormant: its only
// callers were the routes replaced here; the vocabulary port is AU-02).
// ─────────────────────────────────────────────────────────────────────────────

/// The reference trigger vocabulary (shared/schemas.ts:502).
pub const MAIN_TRIGGERS: [&str; 4] = [
    "new_conversation",
    "customer_reply",
    "ai_low_confidence",
    "manual",
];

/// The reference action-kind vocabulary (shared/schemas.ts:528-538).
pub const MAIN_ACTION_KINDS: [&str; 9] = [
    "analyze_ticket",
    "search_similar",
    "check_known_issues",
    "create_ai_note",
    "create_ai_draft",
    "add_tag",
    "set_status",
    "assign",
    "manual_review_queue",
];

/// The reference condition-field vocabulary (shared/schemas.ts:510).
pub const MAIN_CONDITION_FIELDS: [&str; 8] = [
    "subject",
    "body",
    "tag",
    "mailbox",
    "confidence",
    "known_issue_match",
    "ai_attribute",
    "ai_verification",
];

/// The reference condition-operator vocabulary (shared/schemas.ts:511).
pub const MAIN_CONDITION_OPERATORS: [&str; 7] =
    ["contains", "equals", "not_equals", "gt", "gte", "lt", "lte"];

/// The AI attribute catalog keys a `field: 'ai_attribute'` condition may
/// target (shared/constants.ts:114-129 — closed catalog).
pub const MAIN_AI_ATTRIBUTE_KEYS: [&str; 14] = [
    "intent",
    "product",
    "feature",
    "issue",
    "urgency",
    "frustration_cues",
    "technical_familiarity",
    "customer_goal",
    "question_count",
    "risk",
    "known_issue",
    "issue_cluster",
    "response_style",
    "escalation_signal",
];

/// The reference risk-tier table (engine.ts:35-45): read /
/// non_destructive / higher_risk per action kind. Unknown kinds read as
/// 'read' in the reference (`RISK_TIERS[kind] ?? 'read'`).
#[must_use]
pub fn risk_tier(kind: &str) -> Option<&'static str> {
    match kind {
        "analyze_ticket" | "search_similar" | "check_known_issues" => Some("read"),
        "create_ai_note" | "create_ai_draft" | "add_tag" | "manual_review_queue" => {
            Some("non_destructive")
        }
        "set_status" | "assign" => Some("higher_risk"),
        _ => None,
    }
}

/// Complete the automation_rules / automation_runs tables with the columns
/// the reference schema has and M007 lacked (guarded, idempotent — the
/// bootstrap `ensure` pattern). `automation_rules.enabled` keeps M007's
/// DEFAULT 1 for raw inserts, but every insert through the validated CRUD
/// surface stores the zod default `false` explicitly ("disabled by default —
/// enable it when ready").
///
/// # Errors
///
/// Returns `Error::Sqlite` if the DDL fails.
pub fn ensure_main_rule_columns(conn: &Connection) -> Result<()> {
    add_column_if_missing(
        conn,
        "automation_rules",
        "conditions",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(conn, "automation_rules", "priority", "INTEGER DEFAULT 100")?;
    add_column_if_missing(
        conn,
        "automation_rules",
        "requires_approval",
        "INTEGER DEFAULT 1",
    )?;
    add_column_if_missing(conn, "automation_rules", "last_run_at", "TEXT")?;
    add_column_if_missing(conn, "automation_rules", "run_count", "INTEGER DEFAULT 0")?;
    // The reference automation_runs carries a human-readable detail string
    // (engine.ts record()); the port's M035 table lacks the column.
    crate::intelligence_features::apply_m035(conn)?;
    add_column_if_missing(conn, "automation_runs", "detail", "TEXT")
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let exists: bool = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == column);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

/// One zod-style validation issue: a dot-joined path + message, mirroring
/// `{ path: [...], message }` from the reference's ZodError issues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub path: String,
    pub message: String,
}

impl ValidationIssue {
    fn new(path: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_string(),
            message: message.into(),
        }
    }
}

/// A validated rule in the reference `automationRuleSchema` shape — the
/// parsed output WITH zod defaults applied (enabled=false, conditions=[],
/// params={}, priority=100, requires_approval=true).
#[derive(Debug, Clone, PartialEq)]
pub struct RuleInput {
    pub name: String,
    pub enabled: bool,
    pub trigger: String,
    /// Validated conditions array (reference shape, `attribute` only on
    /// `ai_attribute` conditions).
    pub conditions: serde_json::Value,
    /// Validated actions array (reference shape, `params` always present).
    pub actions: serde_json::Value,
    pub priority: i64,
    pub requires_approval: bool,
}

/// Render a JSON value the way zod names the received type in messages
/// ("null", "number", "string", "boolean", "array", "object").
fn received(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(_) => "boolean".into(),
        serde_json::Value::Number(_) => "number".into(),
        serde_json::Value::String(_) => "string".into(),
        serde_json::Value::Array(_) => "array".into(),
        serde_json::Value::Object(_) => "object".into(),
    }
}

/// Render a received value inside an enum message — zod prints the literal
/// ('single-quoted string', bare number/boolean).
fn received_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => format!("'{s}'"),
        other => other.to_string(),
    }
}

fn enum_message(expected: &[&str], received_value: &serde_json::Value) -> String {
    let list = expected
        .iter()
        .map(|e| format!("'{e}'"))
        .collect::<Vec<_>>()
        .join(" | ");
    format!(
        "Invalid enum value. Expected {list}, received {}",
        received_literal(received_value)
    )
}

/// Validate a request body against the reference `automationRuleSchema`
/// (shared/schemas.ts:499-545): same fields, same enums, same defaults, same
/// superRefine rules. Collects ALL issues (zod behavior) in schema-key
/// order; `Ok` returns the parsed rule with defaults applied.
///
/// # Errors
///
/// `Err(issues)` — never panics.
pub fn validate_rule(
    body: &serde_json::Value,
) -> std::result::Result<RuleInput, Vec<ValidationIssue>> {
    let mut issues: Vec<ValidationIssue> = Vec::new();
    let obj: &serde_json::Map<String, serde_json::Value> = match body.as_object() {
        Some(o) => o,
        None => {
            issues.push(ValidationIssue::new(
                "",
                format!("Expected object, received {}", received(body)),
            ));
            return Err(issues);
        }
    };

    // ---- name: z.string().min(1) (required) --------------------------------
    let name = match obj.get("name") {
        None => {
            issues.push(ValidationIssue::new("name", "Required"));
            String::new()
        }
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                issues.push(ValidationIssue::new(
                    "name",
                    "String must contain at least 1 character(s)",
                ));
            }
            s.clone()
        }
        Some(other) => {
            issues.push(ValidationIssue::new(
                "name",
                format!("Expected string, received {}", received(other)),
            ));
            String::new()
        }
    };

    // ---- enabled: z.boolean().default(false) --------------------------------
    let enabled = match obj.get("enabled") {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(other) => {
            issues.push(ValidationIssue::new(
                "enabled",
                format!("Expected boolean, received {}", received(other)),
            ));
            false
        }
    };

    // ---- trigger: z.enum([...4]) (required) ---------------------------------
    let trigger = match obj.get("trigger") {
        None => {
            issues.push(ValidationIssue::new("trigger", "Required"));
            String::new()
        }
        Some(serde_json::Value::String(s)) if MAIN_TRIGGERS.contains(&s.as_str()) => s.clone(),
        Some(other) => {
            issues.push(ValidationIssue::new(
                "trigger",
                enum_message(&MAIN_TRIGGERS, other),
            ));
            String::new()
        }
    };

    // ---- conditions: z.array(conditionSchema).default([]) --------------------
    let conditions = match obj.get("conditions") {
        None => serde_json::Value::Array(Vec::new()),
        Some(serde_json::Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let base = format!("conditions.{i}");
                match item {
                    serde_json::Value::Object(_) => {}
                    serde_json::Value::Null => {
                        issues.push(ValidationIssue::new(
                            &base,
                            "Expected object, received null",
                        ));
                        continue;
                    }
                    other => {
                        issues.push(ValidationIssue::new(
                            &base,
                            format!("Expected object, received {}", received(other)),
                        ));
                        continue;
                    }
                }
                // field / operator / value / attribute
                let cond_field = match item_field(item, "field") {
                    FieldGet::Missing => {
                        issues.push(ValidationIssue::new(&format!("{base}.field"), "Required"));
                        String::new()
                    }
                    FieldGet::Value(serde_json::Value::String(s))
                        if MAIN_CONDITION_FIELDS.contains(&s.as_str()) =>
                    {
                        s.clone()
                    }
                    FieldGet::Value(other) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.field"),
                            enum_message(&MAIN_CONDITION_FIELDS, other),
                        ));
                        String::new()
                    }
                };
                let operator = match item_field(item, "operator") {
                    FieldGet::Missing => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.operator"),
                            "Required",
                        ));
                        String::new()
                    }
                    FieldGet::Value(serde_json::Value::String(s))
                        if MAIN_CONDITION_OPERATORS.contains(&s.as_str()) =>
                    {
                        s.clone()
                    }
                    FieldGet::Value(other) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.operator"),
                            enum_message(&MAIN_CONDITION_OPERATORS, other),
                        ));
                        String::new()
                    }
                };
                let value = match item_field(item, "value") {
                    FieldGet::Missing => {
                        issues.push(ValidationIssue::new(&format!("{base}.value"), "Required"));
                        String::new()
                    }
                    FieldGet::Value(serde_json::Value::String(s)) => {
                        if s.chars().count() > 200 {
                            issues.push(ValidationIssue::new(
                                &format!("{base}.value"),
                                "String must contain at most 200 character(s)",
                            ));
                        }
                        s.clone()
                    }
                    FieldGet::Value(other) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.value"),
                            format!("Expected string, received {}", received(other)),
                        ));
                        String::new()
                    }
                };
                let attribute = match item_field(item, "attribute") {
                    FieldGet::Missing => None,
                    FieldGet::Value(serde_json::Value::Null) => None,
                    FieldGet::Value(serde_json::Value::String(s))
                        if MAIN_AI_ATTRIBUTE_KEYS.contains(&s.as_str()) =>
                    {
                        Some(s.clone())
                    }
                    FieldGet::Value(other) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.attribute"),
                            enum_message(&MAIN_AI_ATTRIBUTE_KEYS, other),
                        ));
                        None
                    }
                };
                // superRefine (schemas.ts:515-522) — runs on every parsed object.
                if cond_field == "ai_attribute" && attribute.is_none() {
                    issues.push(ValidationIssue::new(
                        &format!("{base}.attribute"),
                        "field 'ai_attribute' requires the catalog attribute key (e.g. urgency).",
                    ));
                }
                if cond_field == "ai_verification"
                    && !["failed", "passed", "none"].contains(&value.as_str())
                {
                    issues.push(ValidationIssue::new(
                        &format!("{base}.value"),
                        "field 'ai_verification' value must be 'failed', 'passed' or 'none'.",
                    ));
                }
                let mut c = serde_json::Map::new();
                c.insert("field".into(), serde_json::Value::String(cond_field));
                c.insert("operator".into(), serde_json::Value::String(operator));
                c.insert("value".into(), serde_json::Value::String(value));
                if let Some(a) = attribute {
                    c.insert("attribute".into(), serde_json::Value::String(a));
                }
                out.push(serde_json::Value::Object(c));
            }
            serde_json::Value::Array(out)
        }
        Some(other) => {
            issues.push(ValidationIssue::new(
                "conditions",
                format!("Expected array, received {}", received(other)),
            ));
            serde_json::Value::Array(Vec::new())
        }
    };

    // ---- actions: z.array(actionSchema).min(1) (required) --------------------
    let actions = match obj.get("actions") {
        None => {
            issues.push(ValidationIssue::new("actions", "Required"));
            serde_json::Value::Array(Vec::new())
        }
        Some(serde_json::Value::Array(items)) => {
            if items.is_empty() {
                issues.push(ValidationIssue::new(
                    "actions",
                    "Array must contain at least 1 element(s)",
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let base = format!("actions.{i}");
                match item {
                    serde_json::Value::Null => {
                        issues.push(ValidationIssue::new(
                            &base,
                            "Expected object, received null",
                        ));
                        continue;
                    }
                    serde_json::Value::Object(_) => {}
                    other => {
                        issues.push(ValidationIssue::new(
                            &base,
                            format!("Expected object, received {}", received(other)),
                        ));
                        continue;
                    }
                }
                let kind = match item_field(item, "kind") {
                    FieldGet::Missing => {
                        issues.push(ValidationIssue::new(&format!("{base}.kind"), "Required"));
                        String::new()
                    }
                    FieldGet::Value(serde_json::Value::String(s))
                        if MAIN_ACTION_KINDS.contains(&s.as_str()) =>
                    {
                        s.clone()
                    }
                    FieldGet::Value(other) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.kind"),
                            enum_message(&MAIN_ACTION_KINDS, other),
                        ));
                        String::new()
                    }
                };
                // params: z.record(z.string()).default({})
                let params = match obj_get(item, "params") {
                    None => serde_json::Map::new(),
                    Some(serde_json::Value::Null) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.params"),
                            "Expected object, received null",
                        ));
                        serde_json::Map::new()
                    }
                    Some(serde_json::Value::Object(map)) => {
                        for (k, pv) in map {
                            if !pv.is_string() {
                                issues.push(ValidationIssue::new(
                                    &format!("{base}.params.{k}"),
                                    format!("Expected string, received {}", received(pv)),
                                ));
                            }
                        }
                        map.clone()
                    }
                    Some(other) => {
                        issues.push(ValidationIssue::new(
                            &format!("{base}.params"),
                            format!("Expected object, received {}", received(other)),
                        ));
                        serde_json::Map::new()
                    }
                };
                let mut a = serde_json::Map::new();
                a.insert("kind".into(), serde_json::Value::String(kind));
                a.insert("params".into(), serde_json::Value::Object(params));
                out.push(serde_json::Value::Object(a));
            }
            serde_json::Value::Array(out)
        }
        Some(other) => {
            issues.push(ValidationIssue::new(
                "actions",
                format!("Expected array, received {}", received(other)),
            ));
            serde_json::Value::Array(Vec::new())
        }
    };

    // ---- priority: z.number().int().default(100) -----------------------------
    let priority = match obj.get("priority") {
        None => 100,
        Some(v @ serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.abs() < 9.0e15 {
                    f as i64
                } else {
                    issues.push(ValidationIssue::new(
                        "priority",
                        "Expected int, received float",
                    ));
                    100
                }
            } else {
                issues.push(ValidationIssue::new(
                    "priority",
                    format!("Expected number, received {}", received(v)),
                ));
                100
            }
        }
        Some(other) => {
            issues.push(ValidationIssue::new(
                "priority",
                format!("Expected number, received {}", received(other)),
            ));
            100
        }
    };

    // ---- requires_approval: z.boolean().default(true) ------------------------
    let requires_approval = match obj.get("requires_approval") {
        None => true,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(other) => {
            issues.push(ValidationIssue::new(
                "requires_approval",
                format!("Expected boolean, received {}", received(other)),
            ));
            true
        }
    };

    if issues.is_empty() {
        Ok(RuleInput {
            name,
            enabled,
            trigger,
            conditions,
            actions,
            priority,
            requires_approval,
        })
    } else {
        Err(issues)
    }
}

enum FieldGet<'a> {
    Missing,
    Value(&'a serde_json::Value),
}

fn item_field<'a>(item: &'a serde_json::Value, key: &str) -> FieldGet<'a> {
    match item.as_object().and_then(|o| o.get(key)) {
        None => FieldGet::Missing,
        Some(v) => FieldGet::Value(v),
    }
}

fn obj_get<'a>(item: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    item.as_object().and_then(|o| o.get(key))
}

/// A stored rule in the reference `AutomationRule` shape (engine.ts
/// listRules:47-60): booleans as 0/1, conditions/actions as parsed arrays,
/// plus last_run_at / run_count.
#[derive(Debug, Clone)]
pub struct RuleRecord {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub trigger: String,
    pub conditions: serde_json::Value,
    pub actions: serde_json::Value,
    pub priority: i64,
    pub requires_approval: bool,
    pub last_run_at: Option<String>,
    pub run_count: i64,
}

impl RuleRecord {
    /// The reference listRules payload (engine.ts:47-60 + AutomationRule
    /// type): enabled / requires_approval served as 0|1 ints.
    #[must_use]
    pub fn to_main_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "enabled": if self.enabled { 1 } else { 0 },
            "trigger": self.trigger,
            "conditions": self.conditions,
            "actions": self.actions,
            "priority": self.priority,
            "requires_approval": if self.requires_approval { 1 } else { 0 },
            "last_run_at": self.last_run_at,
            "run_count": self.run_count,
        })
    }
}

const RULE_RECORD_SELECT: &str =
    "SELECT id, name, trigger_json, action_json, COALESCE(enabled, 0), conditions,
            COALESCE(priority, 0), COALESCE(requires_approval, 0), last_run_at,
            COALESCE(run_count, 0)
     FROM automation_rules";

/// The automation_rules row shape read at the query boundary (before JSON
/// parsing): id, name, trigger_json, action_json, enabled, conditions,
/// priority, requires_approval, last_run_at, run_count.
type RuleRow = (
    i64,
    String,
    String,
    String,
    i64,
    String,
    i64,
    i64,
    Option<String>,
    i64,
);

fn read_rule_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RuleRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

fn parse_rule_record(row: RuleRow) -> Result<RuleRecord> {
    let (
        id,
        name,
        trigger_json,
        action_json,
        enabled,
        conditions_json,
        priority,
        requires_approval,
        last_run_at,
        run_count,
    ) = row;
    let trigger: String = serde_json::from_str(&trigger_json)
        .map_err(|e| crate::error::Error::Config(format!("trigger deserialization failed: {e}")))?;
    let actions: serde_json::Value = serde_json::from_str(&action_json)
        .map_err(|e| crate::error::Error::Config(format!("actions deserialization failed: {e}")))?;
    let conditions: serde_json::Value = serde_json::from_str(&conditions_json).map_err(|e| {
        crate::error::Error::Config(format!("conditions deserialization failed: {e}"))
    })?;
    Ok(RuleRecord {
        id,
        name,
        enabled: enabled == 1,
        trigger,
        conditions,
        actions,
        priority,
        requires_approval: requires_approval == 1,
        last_run_at,
        run_count,
    })
}

/// Insert a validated rule (reference engine.createRule:62-65). The MAIN
/// trigger string is stored in `trigger_json` (JSON-encoded string) and the
/// actions array in `action_json` — the port's column names (DB-04).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the INSERT fails (surfaced by the routes —
/// the B4 fix: no more swallowed errors).
pub fn create_rule_record(conn: &Connection, input: &RuleInput) -> Result<i64> {
    let trigger_json = serde_json::to_string(&input.trigger)
        .map_err(|e| crate::error::Error::Config(format!("trigger serialization failed: {e}")))?;
    let action_json = serde_json::to_string(&input.actions)
        .map_err(|e| crate::error::Error::Config(format!("actions serialization failed: {e}")))?;
    let conditions_json = serde_json::to_string(&input.conditions).map_err(|e| {
        crate::error::Error::Config(format!("conditions serialization failed: {e}"))
    })?;
    conn.execute(
        "INSERT INTO automation_rules
             (name, trigger_json, action_json, enabled, conditions, priority, requires_approval)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            input.name,
            trigger_json,
            action_json,
            input.enabled as i64,
            conditions_json,
            input.priority,
            input.requires_approval as i64
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Load one rule by id (`None` when missing — the route answers 404).
///
/// # Errors
///
/// Returns `Error::Sqlite`/`Error::Config` on DB or deserialization errors.
pub fn load_rule_record(conn: &Connection, rule_id: i64) -> Result<Option<RuleRecord>> {
    use rusqlite::OptionalExtension;
    let row: Option<RuleRow> = conn
        .query_row(
            &format!("{RULE_RECORD_SELECT} WHERE id = ?1"),
            params![rule_id],
            read_rule_row,
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(parse_rule_record(row)?)),
    }
}

/// List all rules in the reference order (`ORDER BY priority, id`).
///
/// # Errors
///
/// Returns `Error::Sqlite`/`Error::Config` on DB or deserialization errors.
pub fn list_rule_records(conn: &Connection) -> Result<Vec<RuleRecord>> {
    let mut stmt = conn.prepare(&format!("{RULE_RECORD_SELECT} ORDER BY priority, id"))?;
    let rows = stmt
        .query_map([], read_rule_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(parse_rule_record).collect()
}

/// A single SQL bind value for the dynamic PATCH update (the reference
/// binds raw JS values; booleans bind as 0/1, absent≠null).
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

/// The PATCH field set (reference engine.updateRule:67-91: one SET clause
/// per provided field). `None` = field not in the patch.
#[derive(Debug, Default)]
pub struct RuleUpdate {
    pub name: Option<String>,
    pub enabled: Option<i64>,
    pub trigger: Option<String>,
    pub conditions: Option<serde_json::Value>,
    pub actions: Option<serde_json::Value>,
    pub priority: Option<SqlValue>,
    pub requires_approval: Option<i64>,
}

/// Apply a PATCH (dynamic SET per provided field, reference
/// engine.updateRule:67-91). Returns the number of updated rows.
///
/// # Errors
///
/// Returns `Error::Sqlite`/`Error::Config` on DB or serialization errors.
pub fn update_rule_record(conn: &Connection, rule_id: i64, u: &RuleUpdate) -> Result<usize> {
    let mut sets: Vec<String> = Vec::new();
    let mut vals: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(v) = &u.name {
        sets.push(format!("name = ?{}", vals.len() + 1));
        vals.push(rusqlite::types::Value::Text(v.clone()));
    }
    if let Some(v) = u.enabled {
        sets.push(format!("enabled = ?{}", vals.len() + 1));
        vals.push(rusqlite::types::Value::Integer(v));
    }
    if let Some(v) = &u.trigger {
        let trigger_json = serde_json::to_string(v).map_err(|e| {
            crate::error::Error::Config(format!("trigger serialization failed: {e}"))
        })?;
        sets.push(format!("trigger_json = ?{}", vals.len() + 1));
        vals.push(rusqlite::types::Value::Text(trigger_json));
    }
    if let Some(v) = &u.conditions {
        let s = serde_json::to_string(v).map_err(|e| {
            crate::error::Error::Config(format!("conditions serialization failed: {e}"))
        })?;
        sets.push(format!("conditions = ?{}", vals.len() + 1));
        vals.push(rusqlite::types::Value::Text(s));
    }
    if let Some(v) = &u.actions {
        let s = serde_json::to_string(v).map_err(|e| {
            crate::error::Error::Config(format!("actions serialization failed: {e}"))
        })?;
        sets.push(format!("action_json = ?{}", vals.len() + 1));
        vals.push(rusqlite::types::Value::Text(s));
    }
    if let Some(v) = &u.priority {
        sets.push(format!("priority = ?{}", vals.len() + 1));
        vals.push(match v {
            SqlValue::Null => rusqlite::types::Value::Null,
            SqlValue::Int(i) => rusqlite::types::Value::Integer(*i),
            SqlValue::Real(f) => rusqlite::types::Value::Real(*f),
            SqlValue::Text(t) => rusqlite::types::Value::Text(t.clone()),
        });
    }
    if let Some(v) = u.requires_approval {
        sets.push(format!("requires_approval = ?{}", vals.len() + 1));
        vals.push(rusqlite::types::Value::Integer(v));
    }
    if sets.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "UPDATE automation_rules SET {} WHERE id = ?{}",
        sets.join(", "),
        vals.len() + 1
    );
    vals.push(rusqlite::types::Value::Integer(rule_id));
    let n = conn.execute(&sql, rusqlite::params_from_iter(vals))?;
    Ok(n)
}

/// Delete a rule (reference engine.deleteRule:93-95).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the DELETE fails.
pub fn delete_rule_record(conn: &Connection, rule_id: i64) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM automation_rules WHERE id = ?1",
        params![rule_id],
    )?;
    Ok(n)
}

/// An automation run in the reference payload shape (AutomationRunRecord:
/// id, rule_id, conversation_id, triggered_at, status, detail).
#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: i64,
    pub rule_id: i64,
    pub conversation_id: Option<i64>,
    pub triggered_at: String,
    pub status: String,
    pub detail: Option<String>,
}

impl RunRecord {
    #[must_use]
    pub fn to_main_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "rule_id": self.rule_id,
            "conversation_id": self.conversation_id,
            "triggered_at": self.triggered_at,
            "status": self.status,
            "detail": self.detail,
        })
    }

    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            rule_id: r.get(1)?,
            conversation_id: r.get(2)?,
            triggered_at: r.get(3)?,
            status: r.get(4)?,
            detail: r.get(5)?,
        })
    }
}

/// The port's automation_runs column set (M035 + the detail column) mapped
/// to the reference run payload at the query boundary (DB-04).
const RUN_SELECT: &str =
    "SELECT id, rule_id, conversation_id, created_at, outcome, detail FROM automation_runs";

fn record_run(
    conn: &Connection,
    rule_id: i64,
    conversation_id: Option<i64>,
    status: &str,
    detail: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO automation_runs (rule_id, conversation_id, triggered_by, outcome, detail)
         VALUES (?1, ?2, 'manual', ?3, ?4)",
        params![rule_id, conversation_id, status, detail],
    )?;
    Ok(())
}

/// List runs newest-first (reference engine.listRuns: `ORDER BY id DESC
/// LIMIT ?`).
///
/// # Errors
///
/// Returns `Error::Sqlite` if the query fails.
pub fn list_run_records(conn: &Connection, limit: i64) -> Result<Vec<RunRecord>> {
    let mut stmt = conn.prepare(&format!("{RUN_SELECT} ORDER BY id DESC LIMIT ?1"))?;
    let rows = stmt
        .query_map(params![limit], RunRecord::read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// JS truthiness for PATCH passthrough values (the reference binds
/// `patch.enabled ? 1 : 0` etc.).
#[must_use]
pub fn js_truthy(v: &serde_json::Value) -> i64 {
    match v {
        serde_json::Value::Null => 0,
        serde_json::Value::Bool(b) => i64::from(*b),
        serde_json::Value::Number(n) => i64::from(n.as_f64().unwrap_or(0.0) != 0.0),
        serde_json::Value::String(s) => i64::from(!s.is_empty()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => 1,
    }
}

/// Park a higher-risk / gated action as a pending approval (the port's
/// approval tier — the awaiting-approval mechanism AU-04 wires routes for).
fn park_approval(
    conn: &Connection,
    rule_id: i64,
    conversation_id: i64,
    action: &serde_json::Value,
) -> Result<()> {
    let action_json = serde_json::to_string(action)
        .map_err(|e| crate::error::Error::Config(format!("action serialization failed: {e}")))?;
    conn.execute(
        "INSERT INTO automation_approvals (rule_id, conversation_id, proposed_action_json, status)
         VALUES (?1, ?2, ?3, 'pending')",
        params![rule_id, conversation_id, action_json],
    )?;
    Ok(())
}

/// Fire a trigger for a conversation — the reference manual-trigger path
/// (engine.fireTrigger:110-160 behind routes/automation.ts:82-88):
///
/// - `automation_enabled` OFF ⇒ no runs;
/// - unknown conversation (local id, like the reference `WHERE c.id = ?`)
///   ⇒ no runs;
/// - every ENABLED rule with the fired trigger runs, in `priority, id`
///   order: read actions enqueue + record `completed`, non-destructive
///   actions execute (or park when `requires_approval` and write actions
///   are disabled), higher-risk actions always park an approval;
/// - each fired rule records one run per action, bumps `run_count` /
///   `last_run_at`, and the LAST recorded run row is returned (the
///   reference pushes the last run per rule).
///
/// Conditions are stored and validated (AU-01) but evaluated by the AU-02
/// condition-model port — until then rules fire as matched.
///
/// # Errors
///
/// Returns `Error::Sqlite`/`Error::Config` on DB failures (surfaced by the
/// route as 500 — the B4 fix).
pub fn fire_trigger_for_conversation(
    conn: &mut Connection,
    fired: &str,
    conversation_id: i64,
) -> Result<Vec<RunRecord>> {
    if !crate::settings::get_bool(conn, "automation_enabled", false)? {
        return Ok(Vec::new());
    }
    let conversation_exists = conn
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?1",
            params![conversation_id],
            |_| Ok(()),
        )
        .is_ok();
    if !conversation_exists {
        return Ok(Vec::new());
    }
    ensure_main_rule_columns(conn)?;
    let write_enabled = crate::settings::get_bool(conn, "automation_write_actions_enabled", false)?;
    // WK-03 (C6): AI actions only enqueue when the AI backend can run
    // them (the reference gates on aiEnabled) — a queued job with no
    // runnable backend failed permanently before.
    let ai_enabled = !matches!(
        crate::ai_pipeline::backend_from_settings(conn),
        crate::ai_pipeline::AiBackend::Disabled
    );
    let mut runs = Vec::new();
    for rule in list_rule_records(conn)? {
        if !rule.enabled || rule.trigger != fired {
            continue;
        }
        if let Some(actions) = rule.actions.as_array() {
            for action in actions {
                let kind = action
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or_default();
                let params = action
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
                let tier = risk_tier(kind).unwrap_or("read"); // reference: ?? 'read'
                match tier {
                    "read" => {
                        if kind == "analyze_ticket" {
                            if ai_enabled {
                                // Reference executeReadAction: enqueued as a job so
                                // the AI worker handles it with normal retries.
                                let _ = crate::jobs::enqueue_on(
                                    conn,
                                    "ai",
                                    "analyze_ticket",
                                    &format!("{{\"conversationId\":{conversation_id}}}"),
                                    3,
                                );
                                record_run(
                                    conn,
                                    rule.id,
                                    Some(conversation_id),
                                    "completed",
                                    &format!("Executed read action {kind}"),
                                )?;
                            } else {
                                record_run(
                                    conn,
                                    rule.id,
                                    Some(conversation_id),
                                    "skipped",
                                    "AI action analyze_ticket skipped: AI is disabled",
                                )?;
                            }
                        } else {
                            record_run(
                                conn,
                                rule.id,
                                Some(conversation_id),
                                "completed",
                                &format!("Executed read action {kind}"),
                            )?;
                        }
                    }
                    "non_destructive" => {
                        if rule.requires_approval && !write_enabled {
                            park_approval(conn, rule.id, conversation_id, action)?;
                            record_run(
                                conn,
                                rule.id,
                                Some(conversation_id),
                                "awaiting_approval",
                                &format!("Action {kind} requires approval (non-destructive)"),
                            )?;
                        } else {
                            match kind {
                                "create_ai_note" => {
                                    if ai_enabled {
                                        let _ = crate::jobs::enqueue_on(
                                            conn,
                                            "ai",
                                            "create_ai_note",
                                            &format!("{{\"conversationId\":{conversation_id}}}"),
                                            2,
                                        );
                                    } else {
                                        record_run(
                                            conn,
                                            rule.id,
                                            Some(conversation_id),
                                            "skipped",
                                            "AI action create_ai_note skipped: AI is disabled",
                                        )?;
                                    }
                                }
                                "create_ai_draft" => {
                                    if ai_enabled {
                                        let _ = crate::jobs::enqueue_on(
                                            conn,
                                            "ai",
                                            "generate_draft",
                                            &format!("{{\"conversationId\":{conversation_id}}}"),
                                            2,
                                        );
                                    } else {
                                        record_run(
                                            conn,
                                            rule.id,
                                            Some(conversation_id),
                                            "skipped",
                                            "AI action create_ai_draft skipped: AI is disabled",
                                        )?;
                                    }
                                }
                                "add_tag" => {
                                    // The reference routes add_tag through the
                                    // API queue (a Help Scout write); the port
                                    // applies the local tag (the remote write
                                    // path is the SY-10 provider gap).
                                    if let Some(tag) = params.get("tag").and_then(|t| t.as_str()) {
                                        let mut tags =
                                            crate::conversation_ops::read_conversation_tags(
                                                conn,
                                                conversation_id,
                                            );
                                        if !tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                                            tags.push(tag.to_string());
                                            crate::conversation_ops::write_conversation_tags(
                                                conn,
                                                conversation_id,
                                                &tags,
                                            );
                                        }
                                    }
                                }
                                "manual_review_queue" => {
                                    let _ = conn.execute(
                                        "UPDATE conversations SET is_unread = 1 WHERE id = ?1",
                                        params![conversation_id],
                                    );
                                }
                                _ => {}
                            }
                            record_run(
                                conn,
                                rule.id,
                                Some(conversation_id),
                                "completed",
                                &format!("Executed {kind}"),
                            )?;
                        }
                    }
                    _ => {
                        // higher_risk: always requires explicit approval.
                        park_approval(conn, rule.id, conversation_id, action)?;
                        record_run(
                            conn,
                            rule.id,
                            Some(conversation_id),
                            "awaiting_approval",
                            &format!(
                                "Action {kind} is a write action and requires explicit approval"
                            ),
                        )?;
                    }
                }
            }
        }
        conn.execute(
            "UPDATE automation_rules
             SET run_count = COALESCE(run_count, 0) + 1,
                 last_run_at = datetime('now')
             WHERE id = ?1",
            params![rule.id],
        )?;
        // The reference returns the last recorded run row per rule.
        let last: Option<RunRecord> = conn
            .query_row(
                &format!(
                    "{RUN_SELECT} WHERE id = (SELECT MAX(id) FROM automation_runs WHERE rule_id = ?1)"
                ),
                params![rule.id],
                RunRecord::read,
            )
            .ok();
        if let Some(r) = last {
            runs.push(r);
        }
    }
    Ok(runs)
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

    // ---- MAIN rule model (AU-01) -------------------------------------------

    #[test]
    fn main_validate_rule_applies_zod_defaults() {
        let parsed = validate_rule(&serde_json::json!({
            "name": "Escalate refunds",
            "trigger": "manual",
            "actions": [{ "kind": "add_tag" }]
        }))
        .expect("valid with defaults");
        assert!(!parsed.enabled); // z.boolean().default(false)
        assert_eq!(parsed.priority, 100); // z.number().int().default(100)
        assert!(parsed.requires_approval); // z.boolean().default(true)
        assert_eq!(
            parsed.conditions,
            serde_json::json!([]), // z.array(...).default([])
        );
        // params default {} filled in
        assert_eq!(
            parsed.actions,
            serde_json::json!([{ "kind": "add_tag", "params": {} }])
        );
    }

    #[test]
    fn main_validate_rule_full_schema() {
        let parsed = validate_rule(&serde_json::json!({
            "name": "Refund escalation",
            "enabled": true,
            "trigger": "new_conversation",
            "conditions": [
                { "field": "subject", "operator": "contains", "value": "refund" },
                { "field": "ai_attribute", "operator": "equals", "value": "high",
                  "attribute": "urgency" },
                { "field": "ai_verification", "operator": "equals", "value": "failed" }
            ],
            "actions": [
                { "kind": "analyze_ticket", "params": {} },
                { "kind": "add_tag", "params": { "tag": "vip" } }
            ],
            "priority": 5,
            "requires_approval": false
        }))
        .expect("valid");
        assert!(parsed.enabled);
        assert_eq!(parsed.trigger, "new_conversation");
        assert_eq!(parsed.priority, 5);
        assert!(!parsed.requires_approval);
        // attribute only present on the ai_attribute condition
        assert_eq!(parsed.conditions[1]["attribute"], "urgency");
        assert!(parsed.conditions[0].get("attribute").is_none());
    }

    #[test]
    fn main_validate_rule_collects_all_issues_in_zod_phrasing() {
        let issues = validate_rule(&serde_json::json!({
            "name": "",
            "enabled": "yes",
            "trigger": "status_changed",
            "conditions": [{ "field": "subject", "operator": "bogus", "value": "x" }],
            "actions": [],
            "priority": 1.5,
            "requires_approval": 1
        }))
        .expect_err("invalid");
        let msgs: Vec<(String, String)> = issues
            .iter()
            .map(|i| (i.path.clone(), i.message.clone()))
            .collect();
        assert!(msgs.contains(&(
            "name".into(),
            "String must contain at least 1 character(s)".into()
        )));
        assert!(msgs.contains(&("enabled".into(), "Expected boolean, received string".into())));
        assert!(
            msgs.iter()
                .any(|(p, m)| p == "trigger"
                    && m.starts_with(
                        "Invalid enum value. Expected 'new_conversation' | 'customer_reply' | 'ai_low_confidence' | 'manual', received 'status_changed'"
                    )),
            "got: {msgs:?}"
        );
        assert!(msgs.contains(&(
            "conditions.0.operator".into(),
            "Invalid enum value. Expected 'contains' | 'equals' | 'not_equals' | 'gt' | 'gte' | 'lt' | 'lte', received 'bogus'".into()
        )));
        assert!(msgs.contains(&(
            "actions".into(),
            "Array must contain at least 1 element(s)".into()
        )));
        assert!(msgs.contains(&("priority".into(), "Expected int, received float".into())));
        assert!(msgs.contains(&(
            "requires_approval".into(),
            "Expected boolean, received number".into()
        )));
    }

    #[test]
    fn main_validate_rule_super_refines() {
        // ai_attribute without the attribute key
        let issues = validate_rule(&serde_json::json!({
            "name": "x",
            "trigger": "manual",
            "conditions": [{ "field": "ai_attribute", "operator": "equals", "value": "high" }],
            "actions": [{ "kind": "add_tag" }]
        }))
        .expect_err("superRefine must fire");
        assert!(issues.iter().any(|i| i.path == "conditions.0.attribute"
            && i.message
                == "field 'ai_attribute' requires the catalog attribute key (e.g. urgency)."));

        // ai_verification with a non-vocabulary value
        let issues = validate_rule(&serde_json::json!({
            "name": "x",
            "trigger": "manual",
            "conditions": [{ "field": "ai_verification", "operator": "equals", "value": "maybe" }],
            "actions": [{ "kind": "add_tag" }]
        }))
        .expect_err("superRefine must fire");
        assert!(issues.iter().any(|i| i.path == "conditions.0.value"
            && i.message == "field 'ai_verification' value must be 'failed', 'passed' or 'none'."));

        // unknown catalog attribute key
        let issues = validate_rule(&serde_json::json!({
            "name": "x",
            "trigger": "manual",
            "conditions": [{ "field": "ai_attribute", "operator": "equals", "value": "high",
                             "attribute": "mood" }],
            "actions": [{ "kind": "add_tag" }]
        }))
        .expect_err("closed catalog");
        assert!(issues
            .iter()
            .any(|i| i.path == "conditions.0.attribute" && i.message.contains("received 'mood'")));

        // value over 200 chars
        let long = "a".repeat(201);
        let issues = validate_rule(&serde_json::json!({
            "name": "x",
            "trigger": "manual",
            "conditions": [{ "field": "subject", "operator": "contains", "value": long }],
            "actions": [{ "kind": "add_tag" }]
        }))
        .expect_err("max length");
        assert!(issues.iter().any(|i| i.path == "conditions.0.value"
            && i.message == "String must contain at most 200 character(s)"));

        // params values must be strings
        let issues = validate_rule(&serde_json::json!({
            "name": "x",
            "trigger": "manual",
            "actions": [{ "kind": "add_tag", "params": { "tag": 5 } }]
        }))
        .expect_err("record of strings");
        assert!(issues
            .iter()
            .any(|i| i.path == "actions.0.params.tag"
                && i.message == "Expected string, received number"));

        // missing required keys
        let issues = validate_rule(&serde_json::json!({})).expect_err("required");
        assert!(issues
            .iter()
            .any(|i| i.path == "name" && i.message == "Required"));
        assert!(issues
            .iter()
            .any(|i| i.path == "trigger" && i.message == "Required"));
        assert!(issues
            .iter()
            .any(|i| i.path == "actions" && i.message == "Required"));
    }

    #[test]
    fn main_rule_storage_roundtrip_and_ordering() {
        let conn = fresh_db();
        ensure_main_rule_columns(&conn).unwrap();
        let a = create_rule_record(
            &conn,
            &validate_rule(&serde_json::json!({
                "name": "Low prio", "trigger": "manual",
                "actions": [{ "kind": "analyze_ticket" }], "priority": 200
            }))
            .unwrap(),
        )
        .unwrap();
        let b = create_rule_record(
            &conn,
            &validate_rule(&serde_json::json!({
                "name": "High prio", "trigger": "customer_reply",
                "conditions": [{ "field": "subject", "operator": "contains", "value": "refund" }],
                "actions": [{ "kind": "set_status", "params": { "status": "pending" } }],
                "priority": 10, "requires_approval": true
            }))
            .unwrap(),
        )
        .unwrap();
        let rules = list_rule_records(&conn).unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].id, b, "ORDER BY priority, id (reference order)");
        assert_eq!(rules[0].name, "High prio");
        assert_eq!(rules[0].trigger, "customer_reply");
        assert_eq!(rules[0].conditions[0]["value"], "refund");
        assert_eq!(rules[0].actions[0]["params"]["status"], "pending");
        assert!(rules[0].requires_approval);
        assert!(!rules[1].enabled, "created disabled (zod default)");
        assert_eq!(rules[1].priority, 200);

        let loaded = load_rule_record(&conn, a).unwrap().expect("row a");
        assert_eq!(loaded.name, "Low prio");

        // dynamic PATCH (reference updateRule)
        update_rule_record(
            &conn,
            a,
            &RuleUpdate {
                name: Some("Renamed".into()),
                enabled: Some(1),
                trigger: Some("manual".into()),
                priority: Some(SqlValue::Null), // MAIN quirk: raw null binds NULL
                ..RuleUpdate::default()
            },
        )
        .unwrap();
        let patched = load_rule_record(&conn, a).unwrap().unwrap();
        assert_eq!(patched.name, "Renamed");
        assert!(patched.enabled);
        assert_eq!(patched.trigger, "manual");
        assert_eq!(
            patched.priority, 0,
            "NULL priority reads back as 0 (Number(null))"
        );

        assert_eq!(delete_rule_record(&conn, a).unwrap(), 1);
        assert!(load_rule_record(&conn, a).unwrap().is_none());
        assert_eq!(
            delete_rule_record(&conn, a).unwrap(),
            0,
            "idempotent delete"
        );
    }

    #[test]
    fn main_ensure_columns_is_idempotent_and_completes_m007() {
        let conn = fresh_db(); // apply_m007 only
        ensure_main_rule_columns(&conn).unwrap();
        ensure_main_rule_columns(&conn).unwrap(); // idempotent
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(automation_rules)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for c in [
            "conditions",
            "priority",
            "requires_approval",
            "last_run_at",
            "run_count",
        ] {
            assert!(
                cols.contains(&c.to_string()),
                "missing column {c}: {cols:?}"
            );
        }
        let runs_cols: Vec<String> = conn
            .prepare("PRAGMA table_info(automation_runs)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(runs_cols.contains(&"detail".to_string()), "{runs_cols:?}");
    }
}
