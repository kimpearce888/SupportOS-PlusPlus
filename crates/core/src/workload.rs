//! Workload + capacity metrics (M4-T03).
//!
//! Per spec M4: "Team operations: Operations Center, workload and capacity,
//! Notification Center, mentions, side threads, automation."
//!
//! Workload metrics: per-agent assigned / active / resolved-today counts,
//! plus a per-team rollup.
//!
//! Capacity metrics: the rolling 7-day incoming-vs-closing rate (incoming =
//! conversations created in the last 7 days; closing = conversations closed
//! in the last 7 days). Per KNOWN PITFALLS: all timestamp comparisons use
//! `julianday()` — never lexical ISO-8601 against `datetime('now')`.
//!
//! ## Team membership
//!
//! The `teams` table doesn't persist team membership — that comes from Help
//! Scout sync as `HsTeam.member_user_ids` (a `Vec<i64>`). Rather than
//! introduce a `team_members` table for a 1:1 mirror, the per-team rollup
//! function takes the resolved user-ID list as a parameter. The caller
//! (Tauri shell) resolves team → members via the Help Scout provider.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The rolling window for capacity metrics, in days. Per spec M4: "workload
/// and capacity" — the reference repo uses a 7-day rolling window.
pub const CAPACITY_WINDOW_DAYS: i64 = 7;

/// Per-agent workload metrics.
///
/// "Assigned" = total conversations currently assigned to the agent
/// (status != 'closed'). "Active" = subset of assigned that are still
/// active (status = 'active'). "Resolved today" = conversations the agent
/// closed today (closed_at within the current calendar day, computed via
/// `julianday()` per KNOWN PITFALLS).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWorkload {
    /// The agent's `remote_id` (Help Scout user ID).
    pub agent_remote_id: i64,
    /// Conversations assigned to the agent AND not closed.
    pub assigned: u32,
    /// Subset of `assigned` with status = 'active'.
    pub active: u32,
    /// Conversations the agent closed today (calendar-day, via julianday).
    pub resolved_today: u32,
}

/// Per-team workload rollup. Sums the per-agent workloads for a set of
/// agent IDs (resolved by the caller from the team's `member_user_ids`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamWorkload {
    /// The team's `remote_id` (Help Scout team ID).
    pub team_remote_id: i64,
    /// Number of agents in the rollup.
    pub agent_count: u32,
    /// Sum of `AgentWorkload.assigned` across the team.
    pub assigned: u32,
    /// Sum of `AgentWorkload.active` across the team.
    pub active: u32,
    /// Sum of `AgentWorkload.resolved_today` across the team.
    pub resolved_today: u32,
}

/// Capacity metrics over a rolling 7-day window.
///
/// "Incoming" = conversations created in the last 7 days (using
/// `local_created_at`, which is always set; `created_at` may be NULL
/// until Help Scout sync completes). "Closing" = conversations closed
/// in the last 7 days (using `closed_at`, set when status flips to
/// 'closed'). Both compared via `julianday()` per KNOWN PITFALLS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CapacityMetrics {
    /// Conversations created in the last 7 days.
    pub incoming_7d: u32,
    /// Conversations closed in the last 7 days.
    pub closing_7d: u32,
    /// The rolling-window length in days (7 per spec).
    pub window_days: i64,
    /// Incoming rate = `incoming_7d / window_days` (conversations per day).
    pub incoming_rate_per_day: f64,
    /// Closing rate = `closing_7d / window_days` (conversations per day).
    pub closing_rate_per_day: f64,
}

/// Compute the workload for a single agent.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any of the underlying queries fail.
pub fn agent_workload(conn: &Connection, agent_remote_id: i64) -> Result<AgentWorkload> {
    // Assigned = conversations where assignee_id = ? AND status != 'closed'.
    // Note: conversations.assignee_id stores the Help Scout user's *remote_id*
    // (set from sync), not the local row id.
    let assigned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations
         WHERE assignee_id = ?1 AND status != 'closed'",
        params![agent_remote_id],
        |r| r.get(0),
    )?;

    // Active = subset of assigned with status = 'active'.
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations
         WHERE assignee_id = ?1 AND status = 'active'",
        params![agent_remote_id],
        |r| r.get(0),
    )?;

    // Resolved today = closed_at within today's calendar day.
    // Compare via julianday(closed_at) against julianday('now','start of day').
    // Per KNOWN PITFALLS: no lexical ISO-8601 comparison against datetime('now').
    let resolved_today: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations
         WHERE assignee_id = ?1
           AND status = 'closed'
           AND closed_at IS NOT NULL
           AND julianday(closed_at) >= julianday('now', 'start of day')
           AND julianday(closed_at) < julianday('now', '+1 day', 'start of day')",
        params![agent_remote_id],
        |r| r.get(0),
    )?;

    Ok(AgentWorkload {
        agent_remote_id,
        assigned: u32::try_from(assigned).unwrap_or(0),
        active: u32::try_from(active).unwrap_or(0),
        resolved_today: u32::try_from(resolved_today).unwrap_or(0),
    })
}

/// Compute the workload rollup for a team. The caller resolves
/// `team_remote_id` → `member_remote_ids` via `HelpScoutProvider::list_teams`
/// (the `teams` SQLite table doesn't persist membership; it comes from sync).
///
/// # Errors
///
/// Returns `Error::Sqlite` if any per-agent query fails.
pub fn team_workload(
    conn: &Connection,
    team_remote_id: i64,
    member_remote_ids: &[i64],
) -> Result<TeamWorkload> {
    let mut rollup = TeamWorkload {
        team_remote_id,
        agent_count: u32::try_from(member_remote_ids.len()).unwrap_or(0),
        ..Default::default()
    };
    for &agent_remote_id in member_remote_ids {
        let w = agent_workload(conn, agent_remote_id)?;
        rollup.assigned += w.assigned;
        rollup.active += w.active;
        rollup.resolved_today += w.resolved_today;
    }
    Ok(rollup)
}

/// Compute the capacity metrics over a rolling 7-day window.
///
/// Per spec M4: "workload and capacity." The 7-day window matches the
/// reference repo. Per KNOWN PITFALLS: all timestamp comparisons use
/// `julianday()`.
///
/// # Errors
///
/// Returns `Error::Sqlite` if any underlying query fails.
pub fn capacity_metrics(conn: &Connection) -> Result<CapacityMetrics> {
    // Incoming = local_created_at within the last 7 days.
    // `local_created_at` is always set (SQLite default strftime); `created_at`
    // may be NULL until Help Scout sync completes.
    let incoming_7d: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations
         WHERE julianday(local_created_at) >= julianday('now', ?1)",
        params![format!("-{CAPACITY_WINDOW_DAYS} days")],
        |r| r.get(0),
    )?;

    // Closing = closed_at within the last 7 days.
    let closing_7d: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations
         WHERE closed_at IS NOT NULL
           AND status = 'closed'
           AND julianday(closed_at) >= julianday('now', ?1)",
        params![format!("-{CAPACITY_WINDOW_DAYS} days")],
        |r| r.get(0),
    )?;

    let incoming = u32::try_from(incoming_7d).unwrap_or(0);
    let closing = u32::try_from(closing_7d).unwrap_or(0);
    let window = CAPACITY_WINDOW_DAYS as f64;

    Ok(CapacityMetrics {
        incoming_7d: incoming,
        closing_7d: closing,
        window_days: CAPACITY_WINDOW_DAYS,
        incoming_rate_per_day: incoming as f64 / window,
        closing_rate_per_day: closing as f64 / window,
    })
}

// ─── v1.8.0 capacity model + workload snapshot (reference workloadService) ─
//
// The port of `src/server/operations/workloadService.ts` behind the three
// operations routes the v1.x build answered with hardcoded zeros (every
// agent's `assigned_count`/`open_count`/`resolved_today` were literal 0s,
// teams likewise) and a bare first-10-users list on suggested-assignees.
//
// Design decisions carried over verbatim:
// - Capacity is EXPLICIT configuration (default max + per-user overrides +
//   weights) stored in application_settings — never inferred from anything
//   about a person.
// - Pressure: every open conversation contributes its HIGHEST-tier weight
//   (urgent > sla > waiting > open) — no double counting, deterministic.
// - Average active load is an honest APPROXIMATION from created/closed
//   timestamps with the CURRENT assignee; the method notes say so.
// - Suggested assignee is a READ-ONLY recommendation with its reasoning
//   exposed — nothing reassigns automatically.
//
// Column mapping: the reference's `conversations.assignee_local_id` is the
// port's `assignee_id` (the sync resolves remote user ids to local rows
// before the upsert, and the SLA/notification sweeps already read it as a
// local id).

/// The settings key the capacity model is stored under.
pub const CAPACITY_SETTING_KEY: &str = "capacity_model";

/// Weights of the pressure tiers (reference `CapacityWeights`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CapacityWeights {
    /// Weight of one urgent (high/urgent priority) conversation.
    pub urgent: f64,
    /// Weight of one SLA at-risk/breached conversation.
    pub sla: f64,
    /// Weight of one customer-waiting conversation.
    pub waiting: f64,
    /// Weight of every other open conversation.
    pub open: f64,
}

/// The capacity model (reference `CapacityModel`): default max-open per
/// agent, per-LOCAL-user-id overrides and the pressure weights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacityModel {
    /// Default maximum simultaneously-open conversations per agent.
    pub default_max_open: i64,
    /// Per-user overrides keyed by LOCAL user id (digit strings).
    pub per_user_max: std::collections::BTreeMap<String, i64>,
    /// The pressure tier weights.
    pub weights: CapacityWeights,
}

/// Reference `DEFAULT_CAPACITY_MODEL`.
pub fn default_capacity_model() -> CapacityModel {
    CapacityModel {
        default_max_open: 25,
        per_user_max: std::collections::BTreeMap::new(),
        weights: CapacityWeights {
            urgent: 3.0,
            sla: 2.0,
            waiting: 1.5,
            open: 1.0,
        },
    }
}

impl CapacityModel {
    /// The effective max-open for one user (override or default).
    #[must_use]
    pub fn capacity_for(&self, user_id: i64) -> i64 {
        self.per_user_max
            .get(&user_id.to_string())
            .copied()
            .unwrap_or(self.default_max_open)
    }
}

/// `CapacityModelUpdateSchema` as a parse: default_max_open integer 1..=500,
/// per_user_max keys digits-only with integer values 1..=500, each weight a
/// finite number 0..=100. Returns None on any violation.
pub fn parse_capacity_model(value: &serde_json::Value) -> Option<CapacityModel> {
    validate_capacity_model(value).ok()
}

/// The issue-collecting form of the same schema (the route surfaces each
/// issue in the 422 `detail` array, mirroring the reference's
/// `issues.map(i => path: message)`).
pub fn validate_capacity_model(
    value: &serde_json::Value,
) -> std::result::Result<CapacityModel, Vec<String>> {
    let mut issues = Vec::new();
    let Some(obj) = value.as_object() else {
        return Err(vec!["capacity model must be an object.".to_string()]);
    };
    let default_max_open = match obj.get("default_max_open") {
        Some(v) => match v.as_i64() {
            Some(n) if (1..=500).contains(&n) => n,
            _ => {
                issues.push("default_max_open: must be an integer between 1 and 500.".to_string());
                25
            }
        },
        None => {
            issues.push("default_max_open: Required".to_string());
            25
        }
    };
    let mut per_user_max = std::collections::BTreeMap::new();
    match obj.get("per_user_max") {
        Some(serde_json::Value::Object(per_raw)) => {
            for (k, v) in per_raw {
                if k.is_empty() || !k.chars().all(|c| c.is_ascii_digit()) {
                    issues.push(format!(
                        "per_user_max.{k}: keys must be local user ids (digits)."
                    ));
                    continue;
                }
                match v.as_i64() {
                    Some(n) if (1..=500).contains(&n) => {
                        per_user_max.insert(k.clone(), n);
                    }
                    _ => issues.push(format!(
                        "per_user_max.{k}: must be an integer between 1 and 500."
                    )),
                }
            }
        }
        _ => issues.push("per_user_max: Required".to_string()),
    }
    let mut weights = CapacityWeights {
        urgent: 3.0,
        sla: 2.0,
        waiting: 1.5,
        open: 1.0,
    };
    match obj.get("weights") {
        Some(serde_json::Value::Object(w)) => {
            for key in ["urgent", "sla", "waiting", "open"] {
                match w.get(key).and_then(|v| v.as_f64()) {
                    Some(n) if n.is_finite() && (0.0..=100.0).contains(&n) => match key {
                        "urgent" => weights.urgent = n,
                        "sla" => weights.sla = n,
                        "waiting" => weights.waiting = n,
                        _ => weights.open = n,
                    },
                    _ => issues.push(format!(
                        "weights.{key}: must be a number between 0 and 100."
                    )),
                }
            }
        }
        _ => issues.push("weights: Required".to_string()),
    }
    if issues.is_empty() {
        Ok(CapacityModel {
            default_max_open,
            per_user_max,
            weights,
        })
    } else {
        Err(issues)
    }
}

/// Reference `getCapacityModel`: the stored model, or defaults when absent
/// or invalid (hand-edited / older format) — never a guess that 500s.
pub fn get_capacity_model(conn: &Connection) -> CapacityModel {
    crate::settings::get_json::<serde_json::Value>(conn, CAPACITY_SETTING_KEY)
        .ok()
        .flatten()
        .and_then(|v| parse_capacity_model(&v))
        .unwrap_or_else(default_capacity_model)
}

/// Reference `setCapacityModel`: validates (the caller surfaces 422) then
/// stores. Returns Err on validation failure.
pub fn set_capacity_model(
    conn: &Connection,
    model: &CapacityModel,
) -> std::result::Result<(), String> {
    let value = serde_json::to_value(model)
        .map_err(|e| format!("capacity model serialization failed: {e}"))?;
    match parse_capacity_model(&value) {
        Some(_) => {
            crate::settings::set_json(conn, CAPACITY_SETTING_KEY, model).map_err(|e| e.to_string())
        }
        None => Err("capacity model failed validation".to_string()),
    }
}

/// Reference `AgentWorkload` (the v1.8.0 response row).
#[derive(Debug, Clone, Serialize)]
pub struct AgentSnapshot {
    pub user_local_id: i64,
    pub display_name: String,
    pub mention: Option<String>,
    pub role: Option<String>,
    pub availability: Availability,
    pub open_workload: i64,
    pub pending_workload: i64,
    pub customer_waiting_workload: i64,
    pub urgent_workload: i64,
    pub sla_risk_workload: i64,
    pub weighted_load: f64,
    pub capacity: i64,
    pub pressure: f64,
    pub avg_active_load_7d: Option<f64>,
    pub recent_closed_7d: i64,
}

/// Reference availability: the synced Help Scout user status, reported
/// as-is (`unknown` when nothing synced — never guessed).
#[derive(Debug, Clone, Serialize)]
pub struct Availability {
    pub email_status: Option<String>,
    pub chat_status: Option<String>,
    pub source: &'static str,
}

/// Reference `TeamWorkload`.
#[derive(Debug, Clone, Serialize)]
pub struct TeamSnapshot {
    pub team_local_id: i64,
    pub name: String,
    pub member_user_local_ids: Vec<i64>,
    pub open_workload: i64,
    pub weighted_load: f64,
    pub capacity: i64,
    pub pressure: f64,
    pub recent_closed_7d: i64,
    pub available_members: i64,
    pub total_members: i64,
}

/// Reference `WorkloadSnapshotResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadSnapshot {
    pub generated_at: String,
    pub unassigned_work: i64,
    pub agents: Vec<AgentSnapshot>,
    pub teams: Vec<TeamSnapshot>,
    pub capacity_model: CapacityModel,
    pub method_notes: Vec<&'static str>,
}

/// Reference `SuggestedAssigneeResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct SuggestedAssignee {
    pub conversation_id: i64,
    pub conversation_number: Option<i64>,
    pub subject: Option<String>,
    pub supportos_priority: Option<String>,
    pub waiting_minutes: Option<i64>,
    pub suggested_user_local_id: Option<i64>,
    pub suggested_display_name: Option<String>,
    pub suggested_pressure_after: Option<f64>,
    pub suggested_availability: Option<String>,
    pub all_away: bool,
    pub reason: String,
}

/// Round to 2 decimals (the reference's `Math.round(x * 100) / 100`).
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// One agent row from the users mirror (before snapshot assembly).
struct UserRow {
    id: i64,
    first_name: Option<String>,
    last_name: Option<String>,
    mention: Option<String>,
    role: Option<String>,
}

/// Reference `snapshot()` — the full workload read. The SLA-risk tiering
/// consumes the live `sla_alerts` computation (business minutes), keyed by
/// assignee.
pub fn snapshot(conn: &Connection) -> Result<WorkloadSnapshot> {
    let model = get_capacity_model(conn);
    let mut users_stmt = conn.prepare(
        "SELECT id, first_name, last_name, mention, role
           FROM users WHERE deleted_at IS NULL ORDER BY id",
    )?;
    let users: Vec<UserRow> = users_stmt
        .query_map([], |r| {
            Ok(UserRow {
                id: r.get(0)?,
                first_name: r.get(1)?,
                last_name: r.get(2)?,
                mention: r.get(3)?,
                role: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(users_stmt);

    // SLA conversation ids per assignee (the reference buckets
    // slaAlerts().alerts by assignee_local_id).
    let alerts = crate::sla::sla_alerts(conn)?;
    let mut sla_by_assignee: std::collections::HashMap<i64, Vec<i64>> =
        std::collections::HashMap::new();
    for a in &alerts.alerts {
        if let Some(assignee) = a.assignee_local_id {
            sla_by_assignee
                .entry(assignee)
                .or_default()
                .push(a.conversation_id);
        }
    }

    let mut agents = Vec::with_capacity(users.len());
    for user in users {
        let user_id = user.id;
        let display = {
            let name = [user.first_name.as_deref(), user.last_name.as_deref()]
                .iter()
                .filter_map(|p| p.filter(|s| !s.is_empty()))
                .collect::<Vec<_>>()
                .join(" ");
            if name.is_empty() {
                conn.query_row(
                    "SELECT email FROM users WHERE id = ?1",
                    params![user_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| format!("user #{user_id}"))
            } else {
                name
            }
        };
        let availability = availability_of(conn, user_id);
        let counts = agent_counts(conn, user_id)?;
        let capacity = model.capacity_for(user_id);
        let weighted = weighted_load(
            conn,
            user_id,
            &model.weights,
            sla_by_assignee
                .get(&user_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        );
        agents.push(AgentSnapshot {
            user_local_id: user_id,
            display_name: display,
            mention: user.mention,
            role: user.role,
            availability,
            open_workload: counts.open,
            pending_workload: counts.pending,
            customer_waiting_workload: counts.waiting,
            urgent_workload: counts.urgent,
            sla_risk_workload: sla_by_assignee.get(&user_id).map(Vec::len).unwrap_or(0) as i64,
            weighted_load: round2(weighted),
            capacity,
            pressure: if capacity > 0 {
                round2(weighted / capacity as f64)
            } else {
                0.0
            },
            avg_active_load_7d: avg_active_load_7d(conn, user_id),
            recent_closed_7d: counts.closed7d,
        });
    }

    let teams = team_snapshots(conn, &agents, &model);

    let unassigned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations c
          WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL
            AND c.status IN ('active','pending') AND c.assignee_id IS NULL",
        [],
        |r| r.get(0),
    )?;

    Ok(WorkloadSnapshot {
        generated_at: chrono::Utc::now().to_rfc3339(),
        unassigned_work: unassigned,
        agents,
        teams,
        capacity_model: model,
        method_notes: vec![
            "Pressure = weighted open load / capacity. Each open conversation counts once at its highest tier: urgent, SLA risk, customer-waiting, or plain open.",
            "Average active load is an approximation from created/closed timestamps using the CURRENT assignee; assignment changes mid-conversation are not historically reconstructable.",
            "Availability is the Help Scout user status we already sync (email/chat). Agents with no synced status are shown as unknown, never guessed.",
            "Suggested assignee is a read-only recommendation. SupportOS never reassigns automatically unless you explicitly enable an approved automation.",
        ],
    })
}

struct AgentCounts {
    open: i64,
    pending: i64,
    waiting: i64,
    urgent: i64,
    closed7d: i64,
}

/// Reference `agentCounts` — one aggregate row + the 7-day closed count.
fn agent_counts(conn: &Connection, user_id: i64) -> Result<AgentCounts> {
    let (open, pending, waiting, urgent): (i64, i64, i64, i64) = conn.query_row(
        "SELECT
             SUM(CASE WHEN c.status IN ('active','pending') THEN 1 ELSE 0 END),
             SUM(CASE WHEN c.status = 'pending' THEN 1 ELSE 0 END),
             SUM(CASE WHEN c.status = 'active' AND c.customer_waiting_since IS NOT NULL THEN 1 ELSE 0 END),
             SUM(CASE WHEN c.status IN ('active','pending') AND c.supportos_priority IN ('high','urgent') THEN 1 ELSE 0 END)
           FROM conversations c
          WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL
            AND c.assignee_id = ?1",
        params![user_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap_or((0, 0, 0, 0));
    let closed7d: i64 = conn.query_row(
        "SELECT COUNT(*) FROM conversations c
          WHERE c.deleted_at IS NULL AND c.assignee_id = ?1 AND c.status = 'closed'
            AND c.closed_at IS NOT NULL
            AND julianday(c.closed_at) >= julianday('now', '-7 days')",
        params![user_id],
        |r| r.get(0),
    )?;
    Ok(AgentCounts {
        open,
        pending,
        waiting,
        urgent,
        closed7d,
    })
}

/// Reference `weightedLoad` — highest-tier-wins per open conversation
/// (urgent > SLA risk > waiting > plain open), so weights never
/// double-count.
fn weighted_load(
    conn: &Connection,
    user_id: i64,
    weights: &CapacityWeights,
    sla_conversation_ids: &[i64],
) -> f64 {
    let sla: std::collections::HashSet<i64> = sla_conversation_ids.iter().copied().collect();
    let mut stmt = match conn.prepare(
        "SELECT c.id, c.supportos_priority, c.status, c.customer_waiting_since
           FROM conversations c
          WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL
            AND c.assignee_id = ?1 AND c.status IN ('active','pending')",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return 0.0,
    };
    let rows: Vec<(i64, Option<String>, String, Option<String>)> = match stmt
        .query_map(params![user_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => return 0.0,
    };
    let total: f64 = rows
        .into_iter()
        .map(|(id, priority, status, waiting)| {
            if priority
                .as_deref()
                .is_some_and(|p| p == "high" || p == "urgent")
            {
                weights.urgent
            } else if sla.contains(&id) {
                weights.sla
            } else if status == "active" && waiting.is_some() {
                weights.waiting
            } else {
                weights.open
            }
        })
        .sum();
    total
}

/// Reference `avgActiveLoad7d`: average per-day open workload over the last
/// 7 UTC days (approximation with the CURRENT assignee — see the class doc).
fn avg_active_load_7d(conn: &Connection, user_id: i64) -> Option<f64> {
    let mut total = 0i64;
    for i in 0..7 {
        let day = chrono::Utc::now() - chrono::Duration::days(i);
        let day = day.format("%Y-%m-%d").to_string();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations c
                  WHERE c.deleted_at IS NULL AND c.assignee_id = ?1
                    AND date(COALESCE(c.remote_created_at, c.created_at, c.local_created_at)) <= date(?2)
                    AND (c.closed_at IS NULL OR date(c.closed_at) > date(?2))",
                params![user_id, day],
                |r| r.get(0),
            )
            .unwrap_or(0);
        total += n;
    }
    Some(round2(total as f64 / 7.0))
}

/// Reference `availabilityOf`: the synced user status, or the honest
/// `unknown` source when nothing synced.
fn availability_of(conn: &Connection, user_id: i64) -> Availability {
    let row = conn
        .query_row(
            "SELECT email_status, chat_status FROM user_statuses WHERE user_local_id = ?1",
            params![user_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok();
    match row {
        Some((email_status, chat_status)) => Availability {
            email_status,
            chat_status,
            source: "user_statuses",
        },
        None => Availability {
            email_status: None,
            chat_status: None,
            source: "unknown",
        },
    }
}

/// Reference `teamWorkloads`: per-team rollups over the member agents
/// (membership from the `team_members` mirror).
fn team_snapshots(
    conn: &Connection,
    agents: &[AgentSnapshot],
    model: &CapacityModel,
) -> Vec<TeamSnapshot> {
    let by_user: std::collections::HashMap<i64, &AgentSnapshot> =
        agents.iter().map(|a| (a.user_local_id, a)).collect();
    let teams: Vec<(i64, String)> = {
        let Ok(mut stmt) =
            conn.prepare("SELECT id, name FROM teams WHERE deleted_at IS NULL ORDER BY name")
        else {
            return Vec::new();
        };
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        rows
    };
    let mut out = Vec::new();
    for (team_id, name) in teams {
        let member_ids: Vec<i64> = {
            let Ok(mut stmt) = conn
                .prepare("SELECT user_id FROM team_members WHERE team_id = ?1 ORDER BY user_id")
            else {
                continue;
            };
            let rows: Vec<i64> = stmt
                .query_map(params![team_id], |r| r.get(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
            rows
        };
        let members: Vec<&AgentSnapshot> = member_ids
            .iter()
            .filter_map(|id| by_user.get(id).copied())
            .collect();
        if members.is_empty() {
            continue;
        }
        let open: i64 = members.iter().map(|m| m.open_workload).sum();
        let weighted: f64 = members.iter().map(|m| m.weighted_load).sum();
        let capacity: i64 = members
            .iter()
            .map(|m| model.capacity_for(m.user_local_id))
            .sum();
        out.push(TeamSnapshot {
            team_local_id: team_id,
            name,
            member_user_local_ids: member_ids,
            open_workload: open,
            weighted_load: round2(weighted),
            capacity,
            pressure: if capacity > 0 {
                round2(weighted / capacity as f64)
            } else {
                0.0
            },
            recent_closed_7d: members.iter().map(|m| m.recent_closed_7d).sum(),
            available_members: members
                .iter()
                .filter(|m| {
                    m.availability.email_status.as_deref() == Some("active")
                        || m.availability.chat_status.as_deref() == Some("active")
                })
                .count() as i64,
            total_members: members.len() as i64,
        });
    }
    out
}

/// One unassigned-conversation row for the suggestion read.
struct UnassignedRow {
    id: i64,
    number: Option<i64>,
    subject: Option<String>,
    priority: Option<String>,
    waiting_since: Option<String>,
}

/// Reference `suggestedAssignees`: the top unassigned conversations (urgent
/// first, then longest-waiting) with a read-only suggested assignee each.
/// Deterministic: candidates ranked by (availability, resulting pressure,
/// user id).
pub fn suggested_assignees(conn: &Connection, limit: u32) -> Result<Vec<SuggestedAssignee>> {
    let model = get_capacity_model(conn);
    let snap = snapshot(conn)?;
    let agents = snap.agents;
    let limit = limit.clamp(1, 50) as i64;
    let mut rows_stmt = conn.prepare(
        "SELECT c.id, c.number, c.subject, c.supportos_priority, c.customer_waiting_since
           FROM conversations c
          WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL
            AND c.status IN ('active','pending') AND c.assignee_id IS NULL
          ORDER BY CASE c.supportos_priority
                     WHEN 'urgent' THEN 0 WHEN 'high' THEN 1
                     WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END,
                   c.customer_waiting_since IS NULL, c.customer_waiting_since
          LIMIT ?1",
    )?;
    let rows: Vec<UnassignedRow> = rows_stmt
        .query_map(params![limit], |r| {
            Ok(UnassignedRow {
                id: r.get(0)?,
                number: r.get(1)?,
                subject: r.get(2)?,
                priority: r.get(3)?,
                waiting_since: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(rows_stmt);

    Ok(rows
        .into_iter()
        .map(|row| {
            let conv_id = row.id;
            let priority = row.priority;
            let is_urgent = matches!(priority.as_deref(), Some("urgent") | Some("high"));
            let convo_weight = if is_urgent {
                model.weights.urgent
            } else {
                model.weights.open
            };
            let mut best: Option<(&AgentSnapshot, f64)> = None;
            let mut best_available: Option<(&AgentSnapshot, f64)> = None;
            for a in &agents {
                let after =
                    round2((a.weighted_load + convo_weight) / f64::max(1.0, a.capacity as f64));
                let is_away = a.availability.email_status.as_deref() == Some("away")
                    && a.availability.chat_status.as_deref().unwrap_or("away") == "away";
                let better = |cur: Option<(&AgentSnapshot, f64)>| match cur {
                    Some((agent, value)) => {
                        after < value || (after == value && a.user_local_id < agent.user_local_id)
                    }
                    None => true,
                };
                if better(best) {
                    best = Some((a, after));
                }
                if !is_away && better(best_available) {
                    best_available = Some((a, after));
                }
            }
            let chosen = best_available.or(best);
            let all_away = best_available.is_none();
            let waiting_minutes = row.waiting_since.as_deref().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| {
                    (chrono::Utc::now() - t.with_timezone(&chrono::Utc))
                        .num_minutes()
                        .max(0)
                })
            });
            let reason = match chosen {
                Some((_agent, after)) => format!(
                    "Lowest resulting pressure ({after}){}.",
                    if all_away {
                        " - all agents are away, least-loaded picked"
                    } else {
                        " among available agents"
                    }
                ),
                None => "No agents synced.".to_string(),
            };
            SuggestedAssignee {
                conversation_id: conv_id,
                conversation_number: row.number,
                subject: row.subject,
                supportos_priority: priority,
                waiting_minutes,
                suggested_user_local_id: chosen.map(|(a, _)| a.user_local_id),
                suggested_display_name: chosen.map(|(a, _)| a.display_name.clone()),
                suggested_pressure_after: chosen.map(|(_, after)| after),
                suggested_availability: chosen.map(|(a, _)| {
                    format!(
                        "{} / {}",
                        a.availability.email_status.as_deref().unwrap_or("unknown"),
                        a.availability.chat_status.as_deref().unwrap_or("unknown")
                    )
                }),
                all_away,
                reason,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::apply_m003;
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
        conn
    }

    fn insert_conversation(
        conn: &Connection,
        remote_id: i64,
        status: &str,
        mailbox_id: i64,
        assignee_id: Option<i64>,
        closed_at: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO conversations
                (remote_id, number, status, mailbox_id, customer_id, assignee_id, closed_at)
             VALUES (?1, ?2, ?3, ?4, 2001, ?5, ?6)",
            params![
                remote_id,
                remote_id,
                status,
                mailbox_id,
                assignee_id,
                closed_at
            ],
        )
        .unwrap();
    }

    fn set_local_created_at(conn: &Connection, remote_id: i64, ts: &str) {
        conn.execute(
            "UPDATE conversations SET local_created_at = ?1 WHERE remote_id = ?2",
            params![ts, remote_id],
        )
        .unwrap();
    }

    // ---- Empty dataset ------------------------------------------------------

    #[test]
    fn agent_workload_on_empty_db_returns_zero() {
        let conn = fresh_db();
        let w = agent_workload(&conn, 42).unwrap();
        assert_eq!(w.agent_remote_id, 42);
        assert_eq!(w.assigned, 0);
        assert_eq!(w.active, 0);
        assert_eq!(w.resolved_today, 0);
    }

    #[test]
    fn team_workload_on_empty_members_returns_zero_counts() {
        let conn = fresh_db();
        let t = team_workload(&conn, 7, &[]).unwrap();
        assert_eq!(t.team_remote_id, 7);
        assert_eq!(t.agent_count, 0);
        assert_eq!(t.assigned, 0);
        assert_eq!(t.active, 0);
        assert_eq!(t.resolved_today, 0);
    }

    #[test]
    fn capacity_metrics_on_empty_db_returns_zero() {
        let conn = fresh_db();
        let c = capacity_metrics(&conn).unwrap();
        assert_eq!(c.incoming_7d, 0);
        assert_eq!(c.closing_7d, 0);
        assert_eq!(c.window_days, CAPACITY_WINDOW_DAYS);
        assert_eq!(c.incoming_rate_per_day, 0.0);
        assert_eq!(c.closing_rate_per_day, 0.0);
    }

    // ---- Single-agent workload ---------------------------------------------

    #[test]
    fn agent_workload_counts_assigned_active_resolved_correctly() {
        let conn = fresh_db();
        // Agent 42: 5 conversations.
        // - 3 active (assigned, status='active')
        // - 1 pending (assigned, status='pending') → counts as assigned but not active
        // - 1 closed today (status='closed', closed_at = now) → resolved_today
        let now_iso = iso_now();
        insert_conversation(&conn, 1001, "active", 101, Some(42), None);
        insert_conversation(&conn, 1002, "active", 101, Some(42), None);
        insert_conversation(&conn, 1003, "active", 101, Some(42), None);
        insert_conversation(&conn, 1004, "pending", 101, Some(42), None);
        insert_conversation(&conn, 1005, "closed", 101, Some(42), Some(&now_iso));

        // Conversation assigned to a different agent — must NOT count.
        insert_conversation(&conn, 1006, "active", 101, Some(43), None);

        let w = agent_workload(&conn, 42).unwrap();
        assert_eq!(w.assigned, 4, "3 active + 1 pending (closed excluded)");
        assert_eq!(w.active, 3);
        assert_eq!(w.resolved_today, 1);
    }

    #[test]
    fn agent_workload_excludes_closed_from_assigned() {
        let conn = fresh_db();
        let now_iso = iso_now();
        // 2 closed conversations assigned to agent 42 — must NOT count as assigned.
        insert_conversation(&conn, 1001, "closed", 101, Some(42), Some(&now_iso));
        insert_conversation(&conn, 1002, "closed", 101, Some(42), Some(&now_iso));
        // 1 active — counts.
        insert_conversation(&conn, 1003, "active", 101, Some(42), None);

        let w = agent_workload(&conn, 42).unwrap();
        assert_eq!(w.assigned, 1);
        assert_eq!(w.active, 1);
        assert_eq!(w.resolved_today, 2, "both closed today");
    }

    #[test]
    fn agent_workload_resolved_today_excludes_old_closures() {
        let conn = fresh_db();
        // Closed 30 days ago — should NOT count as resolved_today.
        let old_ts = iso_days_ago(30);
        insert_conversation(&conn, 1001, "closed", 101, Some(42), Some(&old_ts));
        // Closed today — counts.
        let now_iso = iso_now();
        insert_conversation(&conn, 1002, "closed", 101, Some(42), Some(&now_iso));

        let w = agent_workload(&conn, 42).unwrap();
        assert_eq!(w.resolved_today, 1, "only today's closure counts");
        // Both are closed → assigned excludes both.
        assert_eq!(w.assigned, 0);
    }

    #[test]
    fn agent_workload_unassigned_agent_returns_zero() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active", 101, None, None);
        let w = agent_workload(&conn, 42).unwrap();
        assert_eq!(w.assigned, 0);
        assert_eq!(w.active, 0);
    }

    // ---- Team rollup --------------------------------------------------------

    #[test]
    fn team_workload_sums_per_agent_workloads() {
        let conn = fresh_db();
        let now_iso = iso_now();
        // Agent 42: 2 active + 1 closed today.
        insert_conversation(&conn, 1001, "active", 101, Some(42), None);
        insert_conversation(&conn, 1002, "active", 101, Some(42), None);
        insert_conversation(&conn, 1003, "closed", 101, Some(42), Some(&now_iso));
        // Agent 43: 1 active + 1 closed today.
        insert_conversation(&conn, 1004, "active", 101, Some(43), None);
        insert_conversation(&conn, 1005, "closed", 101, Some(43), Some(&now_iso));

        let t = team_workload(&conn, 7, &[42, 43]).unwrap();
        assert_eq!(t.team_remote_id, 7);
        assert_eq!(t.agent_count, 2);
        assert_eq!(t.assigned, 3, "2 + 1 active (closed excluded)");
        assert_eq!(t.active, 3);
        assert_eq!(t.resolved_today, 2, "1 + 1 closed today");
    }

    #[test]
    fn team_workload_with_single_member_matches_agent_workload() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active", 101, Some(42), None);
        insert_conversation(&conn, 1002, "pending", 101, Some(42), None);

        let agent = agent_workload(&conn, 42).unwrap();
        let team = team_workload(&conn, 7, &[42]).unwrap();
        assert_eq!(team.assigned, agent.assigned);
        assert_eq!(team.active, agent.active);
        assert_eq!(team.resolved_today, agent.resolved_today);
    }

    // ---- Capacity metrics ---------------------------------------------------

    #[test]
    fn capacity_metrics_counts_recent_conversations_as_incoming() {
        let conn = fresh_db();
        // 3 conversations created in the last 7 days (default local_created_at = now).
        insert_conversation(&conn, 1001, "active", 101, None, None);
        insert_conversation(&conn, 1002, "active", 101, None, None);
        insert_conversation(&conn, 1003, "active", 101, None, None);
        // 1 conversation created 30 days ago — must NOT count.
        insert_conversation(&conn, 1004, "active", 101, None, None);
        set_local_created_at(&conn, 1004, &iso_days_ago(30));

        let c = capacity_metrics(&conn).unwrap();
        assert_eq!(c.incoming_7d, 3, "only the 3 recent conversations count");
        assert_eq!(c.closing_7d, 0);
        assert_eq!(c.window_days, CAPACITY_WINDOW_DAYS);
        // Rate = 3 / 7 ≈ 0.4286
        assert!((c.incoming_rate_per_day - 3.0 / 7.0).abs() < 1e-9);
        assert_eq!(c.closing_rate_per_day, 0.0);
    }

    #[test]
    fn capacity_metrics_counts_recent_closures_as_closing() {
        let conn = fresh_db();
        let recent_close = iso_days_ago(2);
        let old_close = iso_days_ago(30);
        // 2 closed in last 7 days → count as closing.
        insert_conversation(&conn, 1001, "closed", 101, None, Some(&recent_close));
        insert_conversation(&conn, 1002, "closed", 101, None, Some(&recent_close));
        // 1 closed 30 days ago — must NOT count.
        insert_conversation(&conn, 1003, "closed", 101, None, Some(&old_close));
        // 1 active (not closed) — must NOT count as closing.
        insert_conversation(&conn, 1004, "active", 101, None, None);

        let c = capacity_metrics(&conn).unwrap();
        assert_eq!(c.closing_7d, 2);
        assert!((c.closing_rate_per_day - 2.0 / 7.0).abs() < 1e-9);
    }

    #[test]
    fn capacity_metrics_excludes_unclosed_from_closing_rate() {
        let conn = fresh_db();
        // closed_at set but status != 'closed' → must NOT count as closing.
        let recent = iso_days_ago(2);
        insert_conversation(&conn, 1001, "active", 101, None, Some(&recent));
        let c = capacity_metrics(&conn).unwrap();
        assert_eq!(
            c.closing_7d, 0,
            "status='active' excludes from closing count"
        );
    }

    #[test]
    fn capacity_metrics_window_is_7_days() {
        let conn = fresh_db();
        let c = capacity_metrics(&conn).unwrap();
        assert_eq!(c.window_days, 7);
    }

    // ---- Helpers ------------------------------------------------------------

    /// Returns "now" as an ISO-8601 string for SQLite `closed_at` inserts.
    /// Uses `chrono::Utc::now()` for cross-platform determinism.
    fn iso_now() -> String {
        use chrono::Utc;
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    /// Returns "N days ago" as an ISO-8601 string for SQLite inserts.
    fn iso_days_ago(days: i64) -> String {
        use chrono::{Duration, Utc};
        (Utc::now() - Duration::days(days))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    // ---- v1.8.0 capacity model + snapshot (reference workloadService) -----

    /// The full boot chain — the snapshot reads the M036/M040 columns
    /// (users.deleted_at, conversations.deleted_at/merged_into_conversation_id,
    /// team_members, user_statuses) the legacy test chain never created.
    fn full_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn seed_user(conn: &Connection, remote_id: i64, first: &str, last: &str) -> i64 {
        conn.execute(
            "INSERT INTO users (remote_id, first_name, last_name) VALUES (?1, ?2, ?3)",
            params![remote_id, first, last],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn seed_open_conversation(
        conn: &Connection,
        number: i64,
        assignee: Option<i64>,
        priority: Option<&str>,
        waiting: bool,
    ) -> i64 {
        conn.execute(
            "INSERT INTO conversations
                (remote_id, number, status, mailbox_id, customer_id, assignee_id, supportos_priority,
                 customer_waiting_since, created_at)
             VALUES (?1, ?2, 'active', 1, 3001, ?3, ?4, ?5, ?6)",
            params![
                number,
                number,
                assignee,
                priority,
                if waiting { Some(iso_days_ago(1)) } else { None },
                iso_days_ago(3)
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn default_capacity_model_matches_reference() {
        let m = default_capacity_model();
        assert_eq!(m.default_max_open, 25);
        assert!(m.per_user_max.is_empty());
        assert_eq!(m.weights.urgent, 3.0);
        assert_eq!(m.weights.sla, 2.0);
        assert_eq!(m.weights.waiting, 1.5);
        assert_eq!(m.weights.open, 1.0);
    }

    #[test]
    fn validate_capacity_model_collects_all_issues() {
        let ok = json!({
            "default_max_open": 10,
            "per_user_max": { "7": 3 },
            "weights": { "urgent": 4, "sla": 2, "waiting": 1.5, "open": 1 }
        });
        let model = validate_capacity_model(&ok).unwrap();
        assert_eq!(model.default_max_open, 10);
        assert_eq!(model.per_user_max.get("7"), Some(&3));
        assert_eq!(model.capacity_for(7), 3);
        assert_eq!(model.capacity_for(9), 10);

        let bad = json!({
            "default_max_open": 0,
            "per_user_max": { "abc": 3, "8": 9999 },
            "weights": { "urgent": 400 }
        });
        let issues = validate_capacity_model(&bad).unwrap_err();
        assert!(issues.iter().any(|i| i.contains("default_max_open")));
        assert!(issues.iter().any(|i| i.contains("abc")));
        assert!(issues.iter().any(|i| i.contains("8")));
        assert!(issues.iter().any(|i| i.contains("urgent")));
        assert!(parse_capacity_model(&bad).is_none());
        // Missing everything.
        assert!(validate_capacity_model(&json!({})).is_err());
    }

    #[test]
    fn capacity_model_round_trips_through_settings() {
        let conn = full_db();
        assert_eq!(get_capacity_model(&conn).default_max_open, 25);
        let model = CapacityModel {
            default_max_open: 12,
            per_user_max: [("3".to_string(), 5i64)].into_iter().collect(),
            weights: CapacityWeights {
                urgent: 6.0,
                sla: 2.0,
                waiting: 1.0,
                open: 0.5,
            },
        };
        set_capacity_model(&conn, &model).unwrap();
        let stored = get_capacity_model(&conn);
        assert_eq!(stored, model);
        // An invalid stored value (hand-edited) falls back to defaults,
        // never a 500.
        crate::settings::set_json(&conn, CAPACITY_SETTING_KEY, &json!({"bad": true})).unwrap();
        assert_eq!(get_capacity_model(&conn), default_capacity_model());
    }

    #[test]
    fn snapshot_counts_waiting_urgent_and_unassigned() {
        let conn = full_db();
        let u1 = seed_user(&conn, 501, "Ada", "Lovelace");
        let u2 = seed_user(&conn, 502, "Grace", "Hopper");
        // u1: one urgent + one plain open.
        seed_open_conversation(&conn, 101, Some(u1), Some("urgent"), false);
        seed_open_conversation(&conn, 102, Some(u1), None, false);
        // u2: one customer-waiting.
        seed_open_conversation(&conn, 103, Some(u2), None, true);
        // One unassigned.
        seed_open_conversation(&conn, 104, None, None, false);
        // One closed 2 days ago for u1 (recent_closed_7d).
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, assignee_id, closed_at, created_at)
             VALUES (105, 105, 'closed', 1, 3001, ?1, ?2, ?3)",
            params![u1, iso_days_ago(2), iso_days_ago(5)],
        )
        .unwrap();

        let snap = snapshot(&conn).unwrap();
        assert_eq!(snap.unassigned_work, 1);
        assert_eq!(snap.agents.len(), 2);
        let ada = snap.agents.iter().find(|a| a.user_local_id == u1).unwrap();
        assert_eq!(ada.display_name, "Ada Lovelace");
        assert_eq!(ada.open_workload, 2);
        assert_eq!(ada.urgent_workload, 1);
        assert_eq!(ada.customer_waiting_workload, 0);
        assert_eq!(ada.recent_closed_7d, 1);
        // Weighted: urgent(3) + open(1) = 4 against capacity 25.
        assert_eq!(ada.weighted_load, 4.0);
        assert_eq!(ada.capacity, 25);
        assert_eq!(ada.pressure, round2(4.0 / 25.0));
        assert!(ada.avg_active_load_7d.is_some());
        // Availability: honest unknown when nothing synced.
        assert_eq!(ada.availability.source, "unknown");

        let grace = snap.agents.iter().find(|a| a.user_local_id == u2).unwrap();
        assert_eq!(grace.customer_waiting_workload, 1);
        assert_eq!(grace.weighted_load, 1.5); // waiting tier
        assert_eq!(snap.teams.len(), 0); // no team_members rows
        assert_eq!(snap.method_notes.len(), 4);
    }

    #[test]
    fn snapshot_weighted_load_uses_highest_tier_only() {
        let conn = full_db();
        let u = seed_user(&conn, 601, "Only", "Agent");
        // Urgent AND customer-waiting: counts once at the urgent tier (3),
        // never 3 + 1.5.
        seed_open_conversation(&conn, 201, Some(u), Some("urgent"), true);
        let snap = snapshot(&conn).unwrap();
        let agent = &snap.agents[0];
        assert_eq!(agent.weighted_load, 3.0);
        assert_eq!(agent.open_workload, 1);
        assert_eq!(agent.urgent_workload, 1);
        assert_eq!(agent.customer_waiting_workload, 1);
    }

    #[test]
    fn snapshot_availability_from_synced_statuses() {
        let conn = full_db();
        let u = seed_user(&conn, 701, "Sam", "Cohen");
        conn.execute(
            "INSERT INTO user_statuses (user_local_id, email_status, chat_status)
             VALUES (?1, 'active', 'away')",
            params![u],
        )
        .unwrap();
        let snap = snapshot(&conn).unwrap();
        assert_eq!(snap.agents[0].availability.source, "user_statuses");
        assert_eq!(
            snap.agents[0].availability.email_status.as_deref(),
            Some("active")
        );
        assert_eq!(
            snap.agents[0].availability.chat_status.as_deref(),
            Some("away")
        );
    }

    #[test]
    fn snapshot_team_rollup_over_members() {
        let conn = full_db();
        let u1 = seed_user(&conn, 801, "Ann", "One");
        let u2 = seed_user(&conn, 802, "Bob", "Two");
        conn.execute(
            "INSERT INTO teams (remote_id, name) VALUES (901, 'Triage')",
            [],
        )
        .unwrap();
        let team_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO team_members (team_id, user_id) VALUES (?1, ?2), (?1, ?3)",
            params![team_id, u1, u2],
        )
        .unwrap();
        seed_open_conversation(&conn, 301, Some(u1), None, false);
        seed_open_conversation(&conn, 302, Some(u1), Some("high"), false);
        seed_open_conversation(&conn, 303, Some(u2), None, true);
        conn.execute(
            "INSERT INTO user_statuses (user_local_id, email_status) VALUES (?1, 'active')",
            params![u2],
        )
        .unwrap();

        let snap = snapshot(&conn).unwrap();
        assert_eq!(snap.teams.len(), 1);
        let team = &snap.teams[0];
        assert_eq!(team.name, "Triage");
        assert_eq!(team.member_user_local_ids, vec![u1, u2]);
        assert_eq!(team.open_workload, 3);
        // u1: 3 + 1 = 4; u2: waiting 1.5.
        assert_eq!(team.weighted_load, 5.5);
        assert_eq!(team.capacity, 50);
        assert_eq!(team.available_members, 1);
        assert_eq!(team.total_members, 2);
    }

    #[test]
    fn suggested_assignees_rank_and_reason() {
        let conn = full_db();
        let busy = seed_user(&conn, 1001, "Busy", "Bee");
        let quiet = seed_user(&conn, 1002, "Quiet", "Quail");
        // busy has 5 plain open; quiet has none.
        for n in 401..=405 {
            seed_open_conversation(&conn, n, Some(busy), None, false);
        }
        // Two unassigned: one urgent, one plain.
        let urgent_id = seed_open_conversation(&conn, 406, None, Some("urgent"), true);
        let plain_id = seed_open_conversation(&conn, 407, None, None, false);

        let suggestions = suggested_assignees(&conn, 10).unwrap();
        assert_eq!(suggestions.len(), 2);
        // Urgent first.
        assert_eq!(suggestions[0].conversation_id, urgent_id);
        assert_eq!(suggestions[0].supportos_priority.as_deref(), Some("urgent"));
        assert!(suggestions[0].waiting_minutes.is_some());
        // Both should suggest Quiet (lowest resulting pressure), deterministically.
        for s in &suggestions {
            assert_eq!(s.suggested_user_local_id, Some(quiet));
            assert_eq!(s.suggested_display_name.as_deref(), Some("Quiet Quail"));
            assert!(s.reason.contains("Lowest resulting pressure"));
            assert!(!s.all_away);
        }
        // The urgent conversation's resulting pressure uses the urgent weight.
        let w = get_capacity_model(&conn).weights;
        let expected = round2((0.0 + w.urgent) / 25.0);
        assert_eq!(suggestions[0].suggested_pressure_after, Some(expected));
        assert_eq!(suggestions[1].conversation_id, plain_id);
        // limit clamps.
        assert_eq!(suggested_assignees(&conn, 1).unwrap().len(), 1);
    }

    #[test]
    fn suggested_assignees_all_away_picks_least_loaded() {
        let conn = full_db();
        let a = seed_user(&conn, 1101, "Away", "One");
        let b = seed_user(&conn, 1102, "Also", "Away");
        // Everyone away on both channels.
        conn.execute(
            "INSERT INTO user_statuses (user_local_id, email_status, chat_status)
             VALUES (?1, 'away', 'away'), (?2, 'away', 'away')",
            params![a, b],
        )
        .unwrap();
        seed_open_conversation(&conn, 501, Some(a), None, false);
        let conv = seed_open_conversation(&conn, 502, None, None, false);
        let suggestions = suggested_assignees(&conn, 10).unwrap();
        assert_eq!(suggestions.len(), 1);
        let s = &suggestions[0];
        assert_eq!(s.conversation_id, conv);
        assert!(s.all_away);
        // b is the least-loaded away agent.
        assert_eq!(s.suggested_user_local_id, Some(b));
        assert!(s.reason.contains("all agents are away"));
    }

    #[test]
    fn snapshot_with_no_agents_is_honest_empty() {
        let conn = full_db();
        let snap = snapshot(&conn).unwrap();
        assert_eq!(snap.agents.len(), 0);
        assert_eq!(snap.unassigned_work, 0);
        assert!(suggested_assignees(&conn, 10).unwrap().is_empty());
    }

    use serde_json::json;
}
