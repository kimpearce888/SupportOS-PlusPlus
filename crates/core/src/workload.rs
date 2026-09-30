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
}
