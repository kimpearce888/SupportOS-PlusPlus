//! Operations Center — tile fragments + snapshot aggregator (reference
//! `operations/tileFragments.ts` + `operations/operationsCenter.ts`).
//!
//! Per the reference CHANGELOG v1.7.0: "Operations Center tile count
//! disagreed with the inbox filter list — root cause was two different SQL
//! fragments. Fix: ONE fragment per tile, used by BOTH the tile count and
//! the `GET /api/conversations?ops=<tileKey>` drill-down" — the invariant
//! `tile count == drill-down list total` is structural here.
//!
//! Port column adapters (documented renames, values otherwise verbatim):
//! - `assignee_local_id`→`assignee_id`, `mailbox_local_id`→`mailbox_id`,
//!   `customer_local_id`→`customer_id` (the port's mirror column names);
//! - `known_issue_conversations`→`known_issue_links` (the port's link table
//!   the demo world + issues routes populate);
//! - `ai_runs.output`→`ai_runs.response_json`;
//! - the response-state tiles embed the LIVE `RESPONSE_STATE_SQL` CASE from
//!   `saved_views` (reference responseState.ts), not a stored column — a
//!   snooze boundary can never go stale.
//!
//! The SLA tiles reuse `crate::sla::sla_alerts()` verbatim (reference:
//! "business-minutes logic exists exactly once in the codebase"), so a tile
//! can never disagree with the Issue Radar detail. `ai_escalation` reads the
//! sync-written `ai_runs` table. Every conversation tile count and the
//! drill-down list share [`tile_fragment`].

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::OperationsTileKey;
use crate::error::{Error, Result};
use crate::saved_views::RESPONSE_STATE_SQL;

/// The settings key for the waiting threshold (reference
/// `OPS_WAITING_THRESHOLD_SETTING`).
pub const OPS_WAITING_THRESHOLD_SETTING: &str = "ops_waiting_threshold_minutes";

/// The default waiting threshold: 240 minutes (reference
/// `OPS_WAITING_THRESHOLD_DEFAULT`).
pub const OPS_WAITING_THRESHOLD_DEFAULT: i64 = 240;

/// The conversation-scoped tile keys the inbox drill-down accepts (reference
/// `OPS_TILE_WHITELIST` — the exact 422 message lists them in this order).
pub const OPS_TILE_WHITELIST: [&str; 9] = [
    "unassigned",
    "needs_first_response",
    "customer_waiting",
    "waiting_over_threshold",
    "urgent",
    "high_effort",
    "repeated_issue",
    "known_issue",
    "ai_escalation",
];

/// The exact 422 message an unknown `?ops=` value gets (reference
/// conversations.ts:85 — byte-identical, including the trailing period).
pub const OPS_INVALID_MESSAGE: &str = "ops must be one of: unassigned, needs_first_response, customer_waiting, waiting_over_threshold, urgent, high_effort, repeated_issue, known_issue, ai_escalation.";

/// `true` when `key` is one of the 9 conversation-scoped drill-down tiles
/// (reference `isConversationOpsTile`).
#[must_use]
pub fn is_conversation_ops_tile(key: &str) -> bool {
    OPS_TILE_WHITELIST.contains(&key)
}

/// Conversations this account has closed as spam, deleted or merged away are
/// never operational (reference `NOT_DELETED`).
const NOT_DELETED: &str = "c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL";

/// Build the fragment for a tile key (reference `tileFragment`).
///
/// The SAME parameterized fragment is used by [`snapshot`] for the tile
/// COUNT and by `GET /api/conversations?ops=<key>` for the drill-down list,
/// so a tile number can never disagree with the inbox list it links to.
/// `waiting_threshold_minutes` only affects `waiting_over_threshold`.
/// Non-conversation tiles (SLA, issue spike, jobs, sync, campaigns) have no
/// fragment — the snapshot counts them from their own sources.
#[must_use]
pub fn tile_fragment(
    key: &str,
    waiting_threshold_minutes: i64,
) -> Option<(String, Vec<rusqlite::types::Value>)> {
    let sql: String = match key {
        "unassigned" => {
            format!("{NOT_DELETED} AND c.status IN ('active','pending') AND c.assignee_id IS NULL")
        }
        "needs_first_response" => {
            format!("{NOT_DELETED} AND ({RESPONSE_STATE_SQL}) = 'needs_first_response'")
        }
        "customer_waiting" => {
            format!("{NOT_DELETED} AND ({RESPONSE_STATE_SQL}) = 'customer_waiting'")
        }
        "waiting_over_threshold" => {
            // business-minutes honesty note: the threshold here is WALL minutes
            // (customer_waiting_since is a wall timestamp); the tile says so.
            format!(
                "{NOT_DELETED} AND ({RESPONSE_STATE_SQL}) = 'customer_waiting' \
                 AND c.customer_waiting_since IS NOT NULL \
                 AND (julianday('now') - julianday(c.customer_waiting_since)) * 1440 >= ?"
            )
        }
        "urgent" => format!(
            "{NOT_DELETED} AND c.status IN ('active','pending') \
             AND c.supportos_priority IN ('high','urgent')"
        ),
        "high_effort" => {
            // Honest heuristic (labeled in the UI): strong frustration signal OR
            // the customer had to write >= 5 messages in this conversation.
            format!(
                "{NOT_DELETED} AND c.status IN ('active','pending') AND EXISTS (
          SELECT 1 FROM client_current_signals s
          WHERE s.conversation_id = c.id
            AND (
              EXISTS (SELECT 1 FROM json_each(s.signals_json) je
                      WHERE json_extract(je.value, '$.dimension') = 'frustration'
                        AND json_extract(je.value, '$.value') = 'strong')
              OR CAST(json_extract(s.message_stats_json, '$.customer_messages') AS INTEGER) >= 5
            )
        )"
            )
        }
        "repeated_issue" => {
            // Same customer has >= 2 conversations linked to the SAME known issue.
            format!(
                "{NOT_DELETED} AND c.status IN ('active','pending') AND EXISTS (
          SELECT 1 FROM known_issue_links kic
          WHERE kic.conversation_id = c.id
            AND (SELECT COUNT(*) FROM known_issue_links kic2
                 JOIN conversations c2 ON c2.id = kic2.conversation_id
                 WHERE kic2.known_issue_id = kic.known_issue_id
                   AND c2.customer_id = c.customer_id
                   AND c2.deleted_at IS NULL) >= 2
        )"
            )
        }
        "known_issue" => {
            // Open conversation linked to an UNRESOLVED known issue.
            format!(
                "{NOT_DELETED} AND c.status IN ('active','pending') AND EXISTS (
          SELECT 1 FROM known_issue_links kic
          JOIN known_issues ki ON ki.id = kic.known_issue_id
          WHERE kic.conversation_id = c.id AND ki.status != 'resolved'
        )"
            )
        }
        "ai_escalation" => {
            // Latest completed ticket analysis flags urgency high/critical or
            // frustrated sentiment, with medium/high confidence, ticket still open.
            // (`output`→`response_json` is the port's ai_runs column name.)
            format!(
                "{NOT_DELETED} AND c.status IN ('active','pending') AND EXISTS (
          SELECT 1 FROM ai_runs a
          WHERE a.conversation_id = c.id AND a.type = 'ticket_analysis' AND a.status = 'completed'
            AND a.id = (SELECT MAX(a2.id) FROM ai_runs a2
                        WHERE a2.conversation_id = c.id AND a2.type = 'ticket_analysis' AND a2.status = 'completed')
            AND (json_extract(a.response_json, '$.urgency') IN ('high','critical')
                 OR json_extract(a.response_json, '$.sentiment') = 'frustrated')
            AND json_extract(a.response_json, '$.confidence') IN ('medium','high')
        )"
            )
        }
        _ => return None,
    };
    let params = if key == "waiting_over_threshold" {
        vec![rusqlite::types::Value::Integer(
            waiting_threshold_minutes.max(1),
        )]
    } else {
        Vec::new()
    };
    Some((sql, params))
}

/// One rendered Operations Center tile (reference `OperationsTile`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationsTile {
    /// The tile key (`OperationsTileKey::as_str`).
    pub key: String,
    /// The human label (includes the live threshold for waiting).
    pub label: String,
    /// The tile count.
    pub count: u32,
    /// `info` | `warning` | `critical` (sync_problems is dynamic).
    pub severity: String,
    /// The drill-down target: inbox params or a page name.
    pub drill: Value,
    /// Honest note about the measurement's limits.
    pub note: Option<String>,
}

/// The Operations Center snapshot (reference `OperationsSnapshot`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationsSnapshot {
    /// ISO-8601 UTC build time.
    pub generated_at: String,
    /// The mailbox local ids the snapshot is scoped to, `None` = all.
    pub mailbox_scope: Option<Vec<i64>>,
    /// All 16 tiles in `OperationsTileKey::ALL` order.
    pub tiles: Vec<OperationsTile>,
    /// The threshold `waiting_over_threshold` used (minutes).
    pub waiting_threshold_minutes: i64,
}

impl OperationsSnapshot {
    /// Look up one tile's count by key.
    #[must_use]
    pub fn count_of(&self, key: &str) -> Option<u32> {
        self.tiles.iter().find(|t| t.key == key).map(|t| t.count)
    }
}

/// The waiting threshold in minutes (reference `waitingThresholdMinutes`):
/// the `ops_waiting_threshold_minutes` setting, default 240, clamped to
/// 1..=20160 (two weeks) — non-numeric falls back to the default.
#[must_use]
pub fn waiting_threshold_minutes(conn: &Connection) -> i64 {
    let raw = crate::settings::get_i64(
        conn,
        OPS_WAITING_THRESHOLD_SETTING,
        OPS_WAITING_THRESHOLD_DEFAULT,
    )
    .unwrap_or(OPS_WAITING_THRESHOLD_DEFAULT);
    if (1..=20_160).contains(&raw) {
        raw
    } else {
        OPS_WAITING_THRESHOLD_DEFAULT
    }
}

/// Persist the waiting threshold (reference `setWaitingThresholdMinutes`):
/// clamped to 1..=20160, truncated.
///
/// # Errors
/// Returns [`crate::error::Error::Sqlite`] when the setting write fails.
pub fn set_waiting_threshold_minutes(conn: &Connection, minutes: i64) -> Result<()> {
    let clamped = minutes.clamp(1, 20_160);
    crate::settings::set_i64(conn, OPS_WAITING_THRESHOLD_SETTING, clamped)
}

/// `?mailboxes=1,2` normalization (reference `sanitizeScope`): null/empty →
/// null (all); ids truncated + filtered to positive integers, max 50; an
/// empty result after filtering is null (all) — never a zero-scope snapshot.
fn sanitize_scope(mailbox_ids: Option<&[i64]>) -> Option<Vec<i64>> {
    let ids: Vec<i64> = mailbox_ids?
        .iter()
        .copied()
        .filter(|id| *id > 0)
        .take(50)
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

/// Build the full Operations Center snapshot (reference `snapshot()`).
///
/// Conversation-scoped tiles are COUNT(*) over the SAME whitelisted
/// parameterized fragment the inbox drill-down uses; the SLA tiles reuse
/// the business-minutes engine; the remaining tiles count their own tables.
///
/// # Errors
/// Returns the first SQL/engine error encountered.
pub fn snapshot(conn: &Connection, mailbox_ids: Option<&[i64]>) -> Result<OperationsSnapshot> {
    let scope = sanitize_scope(mailbox_ids);
    let scope_sql = scope.as_ref().map(|ids| {
        format!(
            "c.mailbox_id IN ({})",
            ids.iter().map(|_| "?").collect::<Vec<_>>().join(",")
        )
    });
    let threshold = waiting_threshold_minutes(conn);

    let conv_count = |key: &str| -> Result<u32> {
        // C3 (audit T16): this was a `panic!` on an unknown tile key —
        // with `panic = "abort"` in the release profile it killed the whole
        // packaged app from one operations tile. Return an internal error
        // instead (the route surfaces the standard 500 envelope).
        let (frag_sql, frag_params) = tile_fragment(key, threshold).ok_or_else(|| {
            Error::Config(format!("conversation tile {key} must have a fragment"))
        })?;
        let mut sql = format!("SELECT COUNT(*) FROM conversations c WHERE {frag_sql}");
        let mut params = frag_params;
        if let Some(scope_sql) = scope_sql.as_ref() {
            sql.push_str(" AND ");
            sql.push_str(scope_sql);
            params.extend(
                scope
                    .iter()
                    .flatten()
                    .map(|id| rusqlite::types::Value::Integer(*id)),
            );
        }
        let n: i64 = conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |r| {
            r.get(0)
        })?;
        Ok(u32::try_from(n).unwrap_or(0))
    };

    let inbox_drill = |ops: Option<&str>, view: &str| -> Value {
        match ops {
            Some(ops) => json!({ "type": "inbox", "params": { "view": view, "ops": ops } }),
            None => json!({ "type": "inbox", "params": { "view": view } }),
        }
    };
    let page_drill = |page: &str| -> Value { json!({ "type": "page", "page": page }) };
    let tile = |key: OperationsTileKey,
                label: String,
                count: u32,
                severity: &str,
                drill: Value,
                note: Option<String>| {
        OperationsTile {
            key: key.as_str().to_string(),
            label,
            count,
            severity: severity.to_string(),
            drill,
            note,
        }
    };

    let mut tiles = Vec::with_capacity(OperationsTileKey::ALL.len());

    tiles.push(tile(
        OperationsTileKey::Unassigned,
        "Unassigned".into(),
        conv_count("unassigned")?,
        "info",
        inbox_drill(None, "unassigned"),
        None,
    ));
    tiles.push(tile(
        OperationsTileKey::NeedsFirstResponse,
        "Needs first response".into(),
        conv_count("needs_first_response")?,
        "warning",
        inbox_drill(Some("needs_first_response"), "active"),
        None,
    ));
    tiles.push(tile(
        OperationsTileKey::CustomerWaiting,
        "Customer waiting".into(),
        conv_count("customer_waiting")?,
        "info",
        inbox_drill(Some("customer_waiting"), "active"),
        None,
    ));
    tiles.push(tile(
        OperationsTileKey::WaitingOverThreshold,
        format!("Waiting > {threshold} min"),
        conv_count("waiting_over_threshold")?,
        "warning",
        inbox_drill(Some("waiting_over_threshold"), "active"),
        Some(
            "Wall-clock minutes since the customer's last message (business hours not applied). Threshold is configurable in Settings."
                .to_string(),
        ),
    ));
    tiles.push(tile(
        OperationsTileKey::Urgent,
        "Urgent / high priority".into(),
        conv_count("urgent")?,
        "warning",
        inbox_drill(Some("urgent"), "active"),
        Some("SupportOS priority (local field), high + urgent.".to_string()),
    ));

    // SLA tiles: reuse the business-minutes engine, scoped by the same ids.
    let alerts = crate::sla::sla_alerts(conn)?;
    let scoped_alerts: Vec<&crate::sla::SlaAlertRow> = alerts
        .alerts
        .iter()
        .filter(|a| scope.as_ref().is_none_or(|ids| ids.contains(&a.mailbox_id)))
        .collect();
    let at_risk = scoped_alerts
        .iter()
        .filter(|a| a.state == crate::sla::SlaAlertState::AtRisk)
        .count();
    let breached = scoped_alerts
        .iter()
        .filter(|a| a.state == crate::sla::SlaAlertState::Breached)
        .count();
    tiles.push(tile(
        OperationsTileKey::SlaAtRisk,
        "SLA at risk".into(),
        at_risk as u32,
        "warning",
        page_drill("issues"),
        Some(if alerts.unconfigured_mailboxes.is_empty() {
            "Business minutes at 80%+ of the mailbox target (Issue Radar holds the detail)."
                .to_string()
        } else {
            format!(
                "{} mailbox(es) have no business hours/targets configured and are not monitored.",
                alerts.unconfigured_mailboxes.len()
            )
        }),
    ));
    tiles.push(tile(
        OperationsTileKey::SlaBreached,
        "SLA breached".into(),
        breached as u32,
        "critical",
        page_drill("issues"),
        Some(
            "Business minutes past the mailbox target (Issue Radar holds the detail).".to_string(),
        ),
    ));

    tiles.push(tile(
        OperationsTileKey::HighEffort,
        "High customer effort".into(),
        conv_count("high_effort")?,
        "info",
        inbox_drill(Some("high_effort"), "active"),
        Some(
            "Heuristic: strong frustration signal or 5+ customer messages in the conversation."
                .to_string(),
        ),
    ));
    tiles.push(tile(
        OperationsTileKey::RepeatedIssue,
        "Repeated issue".into(),
        conv_count("repeated_issue")?,
        "info",
        inbox_drill(Some("repeated_issue"), "active"),
        Some("The same customer has 2+ conversations linked to one known issue.".to_string()),
    ));
    tiles.push(tile(
        OperationsTileKey::KnownIssue,
        "Known issue (open tickets)".into(),
        conv_count("known_issue")?,
        "info",
        inbox_drill(Some("known_issue"), "active"),
        Some("Open conversations linked to unresolved known issues.".to_string()),
    ));
    tiles.push(tile(
        OperationsTileKey::AiEscalation,
        "AI escalation".into(),
        conv_count("ai_escalation")?,
        "warning",
        inbox_drill(Some("ai_escalation"), "active"),
        Some(
            "Latest AI analysis: urgency high/critical or frustrated sentiment, medium/high confidence."
                .to_string(),
        ),
    ));
    tiles.push(tile(
        OperationsTileKey::IssueSpike,
        "Issue spike".into(),
        count_issue_spikes(conn)?,
        "warning",
        page_drill("issues"),
        Some("Issue clusters trending up (Issue Radar detail).".to_string()),
    ));
    tiles.push(tile(
        OperationsTileKey::AutomationApprovals,
        "Automation approvals".into(),
        count_automation_approvals(conn)?,
        "warning",
        page_drill("automation"),
        Some("Actions parked awaiting explicit human approval.".to_string()),
    ));
    tiles.push(tile(
        OperationsTileKey::FailedJobs,
        "Failed jobs (7d)".into(),
        count_failed_jobs(conn)?,
        "warning",
        page_drill("automation"),
        Some("Background jobs that exhausted their retries in the last 7 days.".to_string()),
    ));
    let sync_problems = sync_problems(conn)?;
    tiles.push(tile(
        OperationsTileKey::SyncProblems,
        "Sync problems".into(),
        sync_problems.count,
        if sync_problems.state == "ERROR" {
            "critical"
        } else {
            "info"
        },
        page_drill("sync-health"),
        Some(sync_problems.note),
    ));
    tiles.push(tile(
        OperationsTileKey::CampaignActivity,
        "Campaign activity".into(),
        count_campaign_activity(conn)?,
        "info",
        page_drill("outreach"),
        Some("Campaigns queued/sending/paused right now.".to_string()),
    ));

    Ok(OperationsSnapshot {
        generated_at: iso_now(),
        mailbox_scope: scope,
        tiles,
        waiting_threshold_minutes: threshold,
    })
}

/// `new Date().toISOString()` (the snapshot's `generated_at`).
fn iso_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    crate::saved_views::iso_utc_pub(now)
}

/// Issue clusters trending up (reference `countIssueSpikes`).
fn count_issue_spikes(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_clusters WHERE trend = 'rising'",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

/// Parked automation actions awaiting explicit human approval (reference
/// `countAutomationApprovals` — the JOBS table, not a phantom approvals
/// table; this is the audited F-069 divergence fix).
fn count_automation_approvals(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE type = 'automation_action_awaiting_approval' AND status IN ('queued','parked')",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

/// Jobs that exhausted retries in the last 7 days (reference
/// `countFailedJobs`).
fn count_failed_jobs(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE status = 'failed' AND julianday(COALESCE(completed_at, created_at)) >= julianday('now', '-7 days')",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

/// The sync-problems rollup (reference `syncProblems`): sync state ERROR
/// counts 1, plus application errors logged in the last 24 h, with the
/// honest note the tile displays.
fn sync_problems(conn: &Connection) -> Result<SyncProblems> {
    let state: String = conn
        .query_row(
            "SELECT value FROM application_settings WHERE key = 'sync_state'",
            [],
            |r| r.get(0),
        )
        .map(|v: String| v.replace('"', ""))
        .unwrap_or_else(|_| "NEW".to_string());
    let recent_errors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM application_errors WHERE julianday(timestamp) >= julianday('now', '-1 day')",
        [],
        |r| r.get(0),
    )?;
    let recent_errors = u32::try_from(recent_errors).unwrap_or(0);
    let problematic = u32::from(state == "ERROR");
    let count = problematic + recent_errors;
    let note = if state == "ERROR" {
        if recent_errors > 0 {
            format!("Sync state is ERROR plus {recent_errors} application error(s) in 24h.")
        } else {
            "Sync state is ERROR.".to_string()
        }
    } else if recent_errors > 0 {
        format!("{recent_errors} application error(s) logged in the last 24h (state: {state}).")
    } else {
        format!("Sync state: {state}.")
    };
    Ok(SyncProblems { count, state, note })
}

/// The private `syncProblems()` return shape.
struct SyncProblems {
    count: u32,
    state: String,
    note: String,
}

/// Campaigns queued/sending/paused right now (reference
/// `countCampaignActivity`).
fn count_campaign_activity(conn: &Connection) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM outreach_campaigns WHERE status IN ('queued','sending','paused')",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(n).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn
    }

    fn insert_conversation(
        conn: &Connection,
        remote_id: i64,
        status: &str,
        mailbox_id: i64,
        assignee_id: Option<i64>,
        priority: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO conversations
                (remote_id, number, status, mailbox_id, customer_id, assignee_id, supportos_priority)
             VALUES (?1, ?2, ?3, ?4, 2001, ?5, ?6)",
            params![remote_id, remote_id, status, mailbox_id, assignee_id, priority],
        )
        .unwrap();
    }

    /// ISO string `mins` minutes before now (the `toISOString()` shape).
    fn minutes_ago_iso(mins: i64) -> String {
        let ts = chrono::Utc::now().timestamp_millis() - mins * 60_000;
        chrono::TimeZone::timestamp_millis_opt(&chrono::Utc, ts)
            .single()
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    /// Mailbox 101 with a 24/7 UTC schedule, 60-min first-response target;
    /// two waiting conversations aged `breached` / `at_risk` minutes.
    fn seed_sla_fixture(conn: &Connection, breached_mins_ago: i64, at_risk_mins_ago: i64) {
        crate::sla::ensure_sla_schema(conn).unwrap();
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (101, 201, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mailbox_business_hours
                (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
             VALUES (101, 'UTC', '[0,1,2,3,4,5,6]', 0, 1440, 60, 480, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        for (remote, mins) in [(5001, breached_mins_ago), (5002, at_risk_mins_ago)] {
            let at = minutes_ago_iso(mins);
            conn.execute(
                "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, created_at, updated_at)
                 VALUES (?1, ?1, 'active', 101, 2001, ?2, ?2)",
                params![remote, at],
            )
            .unwrap();
            let id: i64 = conn
                .query_row(
                    "SELECT id FROM conversations WHERE remote_id = ?1",
                    [remote],
                    |r| r.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
                 VALUES (?1, 'customer', 'published', 'b', 'customer', ?2)",
                params![id, at],
            )
            .unwrap();
        }
    }

    // ---- Waiting threshold -----------------------------------------------------

    #[test]
    fn waiting_threshold_defaults_to_240() {
        let conn = fresh_db();
        assert_eq!(waiting_threshold_minutes(&conn), 240);
    }

    #[test]
    fn waiting_threshold_setting_respected_and_clamped() {
        let conn = fresh_db();
        crate::settings::set_i64(&conn, OPS_WAITING_THRESHOLD_SETTING, 90).unwrap();
        assert_eq!(waiting_threshold_minutes(&conn), 90);
        // Out-of-range values fall back to the DEFAULT (reference
        // Number.isFinite gate), not to the clamp edge.
        crate::settings::set_i64(&conn, OPS_WAITING_THRESHOLD_SETTING, 0).unwrap();
        assert_eq!(waiting_threshold_minutes(&conn), 240);
        crate::settings::set_i64(&conn, OPS_WAITING_THRESHOLD_SETTING, 99_999).unwrap();
        assert_eq!(waiting_threshold_minutes(&conn), 240);
    }

    #[test]
    fn set_waiting_threshold_persists_clamped() {
        let conn = fresh_db();
        set_waiting_threshold_minutes(&conn, 1_000_000).unwrap();
        assert_eq!(waiting_threshold_minutes(&conn), 20_160);
        set_waiting_threshold_minutes(&conn, 0).unwrap();
        assert_eq!(waiting_threshold_minutes(&conn), 1);
    }

    // ---- Fragments -------------------------------------------------------------

    #[test]
    fn fragments_start_with_not_deleted() {
        for key in OPS_TILE_WHITELIST {
            let (sql, params) = tile_fragment(key, 240).unwrap();
            assert!(
                sql.starts_with("c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL"),
                "{key}: {sql}"
            );
            if key == "waiting_over_threshold" {
                assert_eq!(params.len(), 1, "{key} binds the threshold");
            } else {
                assert!(params.is_empty(), "{key} binds nothing");
            }
        }
    }

    #[test]
    fn non_conversation_keys_have_no_fragment() {
        for key in [
            "sla_at_risk",
            "sla_breached",
            "issue_spike",
            "automation_approvals",
            "failed_jobs",
            "sync_problems",
            "campaign_activity",
            "nonsense",
            "",
        ] {
            assert!(tile_fragment(key, 240).is_none(), "{key}");
        }
    }

    #[test]
    fn urgent_fragment_covers_high_and_urgent() {
        // F-069 fix: urgent = IN ('high','urgent'), not = 'urgent'.
        let (sql, _) = tile_fragment("urgent", 240).unwrap();
        assert!(
            sql.contains("c.supportos_priority IN ('high','urgent')"),
            "{sql}"
        );
    }

    #[test]
    fn waiting_threshold_fragment_binds_minutes_param() {
        let (sql, params) = tile_fragment("waiting_over_threshold", 240).unwrap();
        assert!(sql.contains("* 1440 >= ?"), "{sql}");
        assert_eq!(params, vec![rusqlite::types::Value::Integer(240)]);
        // Math.max(1, trunc(threshold)) — the clamp lives in the fragment.
        let (_, params) = tile_fragment("waiting_over_threshold", 0).unwrap();
        assert_eq!(params, vec![rusqlite::types::Value::Integer(1)]);
    }

    #[test]
    fn high_effort_reads_client_current_signals() {
        // F-069 fix: the reference definition — strong frustration signal OR
        // 5+ customer messages from the interaction snapshot.
        let (sql, _) = tile_fragment("high_effort", 240).unwrap();
        assert!(sql.contains("client_current_signals"), "{sql}");
        assert!(
            sql.contains("json_extract(je.value, '$.dimension') = 'frustration'")
                && sql.contains("json_extract(je.value, '$.value') = 'strong'"),
            "{sql}"
        );
        assert!(
            sql.contains("json_extract(s.message_stats_json, '$.customer_messages')"),
            "{sql}"
        );
    }

    #[test]
    fn whitelist_matches_reference_order() {
        assert_eq!(
            OPS_TILE_WHITELIST,
            [
                "unassigned",
                "needs_first_response",
                "customer_waiting",
                "waiting_over_threshold",
                "urgent",
                "high_effort",
                "repeated_issue",
                "known_issue",
                "ai_escalation"
            ]
        );
        for key in OPS_TILE_WHITELIST {
            assert!(is_conversation_ops_tile(key));
        }
        assert!(!is_conversation_ops_tile("sla_at_risk"));
        assert!(!is_conversation_ops_tile("nope"));
    }

    // ---- Snapshot shape ---------------------------------------------------------

    #[test]
    fn snapshot_has_16_reference_shaped_tiles_in_order() {
        let conn = fresh_db();
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.tiles.len(), 16);
        let keys: Vec<&str> = snap.tiles.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(
            keys,
            OperationsTileKey::ALL
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
        );
        for t in &snap.tiles {
            assert!(!t.label.is_empty(), "{}", t.key);
            assert!(
                matches!(t.severity.as_str(), "info" | "warning" | "critical"),
                "{}: {}",
                t.key,
                t.severity
            );
            assert!(t.drill.is_object(), "{}", t.key);
        }
        assert!(snap.waiting_threshold_minutes >= 1);
        assert!(snap.generated_at.ends_with('Z'));
        assert_eq!(snap.mailbox_scope, None);
    }

    #[test]
    fn snapshot_labels_and_notes_match_reference() {
        let conn = fresh_db();
        let snap = snapshot(&conn, None).unwrap();
        let label = |k: &str| {
            snap.tiles
                .iter()
                .find(|t| t.key == k)
                .unwrap()
                .label
                .clone()
        };
        let note = |k: &str| snap.tiles.iter().find(|t| t.key == k).unwrap().note.clone();
        assert_eq!(label("unassigned"), "Unassigned");
        assert_eq!(label("needs_first_response"), "Needs first response");
        assert_eq!(label("waiting_over_threshold"), "Waiting > 240 min");
        assert_eq!(label("urgent"), "Urgent / high priority");
        assert_eq!(label("sla_breached"), "SLA breached");
        assert_eq!(label("high_effort"), "High customer effort");
        assert_eq!(label("known_issue"), "Known issue (open tickets)");
        assert_eq!(label("failed_jobs"), "Failed jobs (7d)");
        assert!(note("unassigned").is_none());
        assert_eq!(
            note("urgent").as_deref(),
            Some("SupportOS priority (local field), high + urgent.")
        );
        assert_eq!(
            note("automation_approvals").as_deref(),
            Some("Actions parked awaiting explicit human approval.")
        );
        // The threshold flows into the label when the setting changes.
        crate::settings::set_i64(&conn, OPS_WAITING_THRESHOLD_SETTING, 30).unwrap();
        let snap2 = snapshot(&conn, None).unwrap();
        assert_eq!(
            snap2
                .tiles
                .iter()
                .find(|t| t.key == "waiting_over_threshold")
                .unwrap()
                .label,
            "Waiting > 30 min"
        );
    }

    #[test]
    fn snapshot_drill_targets_match_reference() {
        let conn = fresh_db();
        let snap = snapshot(&conn, None).unwrap();
        let drill = |k: &str| {
            snap.tiles
                .iter()
                .find(|t| t.key == k)
                .unwrap()
                .drill
                .clone()
        };
        assert_eq!(
            drill("unassigned"),
            serde_json::json!({"type": "inbox", "params": {"view": "unassigned"}})
        );
        assert_eq!(
            drill("needs_first_response"),
            serde_json::json!({"type": "inbox", "params": {"view": "active", "ops": "needs_first_response"}})
        );
        assert_eq!(
            drill("sla_at_risk"),
            serde_json::json!({"type": "page", "page": "issues"})
        );
        assert_eq!(
            drill("automation_approvals"),
            serde_json::json!({"type": "page", "page": "automation"})
        );
        assert_eq!(
            drill("sync_problems"),
            serde_json::json!({"type": "page", "page": "sync-health"})
        );
        assert_eq!(
            drill("campaign_activity"),
            serde_json::json!({"type": "page", "page": "outreach"})
        );
    }

    #[test]
    fn snapshot_mailbox_scope_sanitized() {
        let conn = fresh_db();
        let snap = snapshot(&conn, Some(&[101, -5, 0])).unwrap();
        // Negative + zero ids are dropped; a non-empty remainder scopes.
        assert_eq!(snap.mailbox_scope, Some(vec![101]));
        // All filtered out -> null (all inboxes), never a zero-scope snapshot.
        let snap = snapshot(&conn, Some(&[-1, 0])).unwrap();
        assert_eq!(snap.mailbox_scope, None);
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.mailbox_scope, None);
    }

    // ---- The v1.7.0 invariant: tile count == fragment count == list total ------

    /// Seed a world that lights up all 9 conversation tiles.
    fn seed_tile_world(conn: &Connection) {
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (101, 201, 'Support')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO customers (id, remote_id) VALUES (2001, 2001)",
            [],
        )
        .unwrap();
        // unassigned + urgent('high') + never touched by agents.
        insert_conversation(conn, 1001, "active", 101, None, Some("high"));
        // needs_first_response: customer wrote, no agent reply ever.
        insert_conversation(conn, 1002, "active", 101, Some(1), None);
        conn.execute(
            "UPDATE conversations SET first_customer_message_at = ?1 WHERE remote_id = 1002",
            params![minutes_ago_iso(600)],
        )
        .unwrap();
        // customer_waiting + waiting_over_threshold (customer_waiting_since 5h old).
        // first_response_at is set so the CASE falls PAST needs_first_response.
        insert_conversation(conn, 1003, "active", 101, Some(1), None);
        conn.execute(
            "UPDATE conversations SET first_customer_message_at = ?1, first_response_at = ?2,
                last_customer_reply_at = ?3, last_human_agent_response_at = ?2, customer_waiting_since = ?3
             WHERE remote_id = 1003",
            params![minutes_ago_iso(600), minutes_ago_iso(400), minutes_ago_iso(300)],
        )
        .unwrap();
        // high_effort: strong frustration in the interaction snapshot.
        insert_conversation(conn, 1004, "active", 101, Some(1), None);
        let c1004: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 1004",
                [],
                |r| r.get(0),
            )
            .unwrap();
        crate::interaction_current::record_current_interaction(conn, c1004).unwrap();
        conn.execute(
            "UPDATE client_current_signals SET signals_json = ?1 WHERE conversation_id = ?2",
            params![
                r#"[{"dimension":"frustration","value":"strong","confidence":"medium","evidence":{"excerpt":"not working again still","thread_local_id":null,"conversation_local_id":null},"source":"heuristic"}]"#,
                c1004
            ],
        )
        .unwrap();
        // known_issue + repeated_issue: same customer, two conversations, one
        // unresolved known issue.
        conn.execute(
            "INSERT INTO known_issues (id, name, status) VALUES (7, 'API outage', 'investigating')",
            [],
        )
        .unwrap();
        for remote in [1005, 1006] {
            insert_conversation(conn, remote, "active", 101, Some(1), None);
            let cid: i64 = conn
                .query_row(
                    "SELECT id FROM conversations WHERE remote_id = ?1",
                    [remote],
                    |r| r.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO known_issue_links (known_issue_id, conversation_id) VALUES (7, ?1)",
                params![cid],
            )
            .unwrap();
        }
        // ai_escalation: latest completed analysis, urgency high, medium confidence.
        insert_conversation(conn, 1007, "active", 101, Some(1), None);
        let c1007: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 1007",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status)
             VALUES ('h', 'v', 'm', ?1, 'ticket_analysis', ?2, 'completed')",
            params![
                r#"{"urgency":"high","sentiment":"neutral","confidence":"medium"}"#,
                c1007
            ],
        )
        .unwrap();
    }

    #[test]
    fn tile_count_equals_fragment_count_equals_inbox_list_total() {
        let conn = fresh_db();
        seed_tile_world(&conn);
        let snap = snapshot(&conn, None).unwrap();
        let threshold = snap.waiting_threshold_minutes;

        for key in OPS_TILE_WHITELIST {
            let tile_count = snap.count_of(key).unwrap_or(u32::MAX);
            // Fragment count.
            let (frag_sql, frag_params) = tile_fragment(key, threshold).unwrap();
            let frag_count: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM conversations c WHERE {frag_sql}"),
                    rusqlite::params_from_iter(frag_params.iter()),
                    |r| r.get(0),
                )
                .unwrap();
            // Drill-down list total (the route's extra_where path).
            let filters = crate::inbox::InboxFilters {
                extra_where: Some(frag_sql.clone()),
                extra_params: frag_params.clone(),
                limit: Some(200),
                ..Default::default()
            };
            let (_, list_total) = crate::inbox::list_conversations(&conn, &filters).unwrap();
            assert_eq!(
                tile_count as i64, frag_count,
                "{key}: tile == fragment count"
            );
            assert_eq!(
                list_total as i64, frag_count,
                "{key}: list total == fragment count"
            );
        }

        // The seeded world actually lights the tiles (the test is not vacuous).
        assert!(snap.count_of("unassigned").unwrap() >= 1);
        assert!(snap.count_of("needs_first_response").unwrap() >= 1);
        assert!(snap.count_of("customer_waiting").unwrap() >= 1);
        assert!(snap.count_of("waiting_over_threshold").unwrap() >= 1);
        assert!(snap.count_of("urgent").unwrap() >= 1);
        assert!(snap.count_of("high_effort").unwrap() >= 1);
        assert!(snap.count_of("repeated_issue").unwrap() >= 1);
        assert!(snap.count_of("known_issue").unwrap() >= 1);
        assert!(snap.count_of("ai_escalation").unwrap() >= 1);
    }

    #[test]
    fn tile_counts_respect_mailbox_scope() {
        let conn = fresh_db();
        seed_tile_world(&conn);
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (102, 202, 'Other')",
            [],
        )
        .unwrap();
        insert_conversation(&conn, 2001, "active", 102, None, Some("urgent"));
        let all = snapshot(&conn, None).unwrap();
        let scoped = snapshot(&conn, Some(&[102])).unwrap();
        assert_eq!(all.count_of("unassigned").unwrap(), 2);
        assert_eq!(scoped.count_of("unassigned").unwrap(), 1);
        assert_eq!(scoped.count_of("urgent").unwrap(), 1);
        assert_eq!(scoped.count_of("high_effort").unwrap(), 0);
    }

    // ---- Non-conversation tiles --------------------------------------------------

    #[test]
    fn automation_approvals_counts_parked_jobs_not_the_phantom_table() {
        let conn = fresh_db();
        // The F-069 fix: the JOBS table is the source, not automation_approvals.
        conn.execute(
            "INSERT INTO jobs (queue, type, status) VALUES ('automation', 'automation_action_awaiting_approval', 'parked')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (queue, type, status) VALUES ('automation', 'automation_action_awaiting_approval', 'queued')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (queue, type, status) VALUES ('automation', 'automation_action_awaiting_approval', 'done')",
            [],
        )
        .unwrap();
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("automation_approvals").unwrap(), 2);
    }

    #[test]
    fn failed_jobs_counts_last_seven_days_only() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO jobs (queue, type, status, created_at) VALUES ('t', 't', 'failed', datetime('now', '-2 days'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (queue, type, status, created_at) VALUES ('t', 't', 'failed', datetime('now', '-30 days'))",
            [],
        )
        .unwrap();
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("failed_jobs").unwrap(), 1);
    }

    #[test]
    fn sync_problems_note_and_dynamic_severity() {
        let conn = fresh_db();
        // No state row -> NEW, no errors.
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("sync_problems").unwrap(), 0);
        let tile = snap
            .tiles
            .iter()
            .find(|t| t.key == "sync_problems")
            .unwrap();
        assert_eq!(tile.note.as_deref(), Some("Sync state: NEW."));
        assert_eq!(tile.severity, "info");

        crate::settings::set_string(&conn, "sync_state", "ERROR").unwrap();
        conn.execute(
            "INSERT INTO application_errors (timestamp, message) VALUES (datetime('now'), 'x')",
            [],
        )
        .unwrap();
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("sync_problems").unwrap(), 2);
        let tile = snap
            .tiles
            .iter()
            .find(|t| t.key == "sync_problems")
            .unwrap();
        assert_eq!(
            tile.note.as_deref(),
            Some("Sync state is ERROR plus 1 application error(s) in 24h.")
        );
        assert_eq!(tile.severity, "critical");
    }

    #[test]
    fn campaign_activity_counts_outreach_campaigns() {
        let conn = fresh_db();
        for (name, status) in [("c1", "queued"), ("c2", "sending"), ("c3", "done")] {
            conn.execute(
                "INSERT INTO outreach_campaigns (name, subject, body, status) VALUES (?1, 's', 'b', ?2)",
                params![name, status],
            )
            .unwrap();
        }
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("campaign_activity").unwrap(), 2);
    }

    #[test]
    fn ai_escalation_requires_latest_completed_medium_confidence() {
        let conn = fresh_db();
        insert_conversation(&conn, 3001, "active", 101, Some(1), None);
        let cid: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 3001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Latest run: low confidence -> NOT escalation even with high urgency.
        conn.execute(
            "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status)
             VALUES ('h1', 'v', 'm', ?1, 'ticket_analysis', ?2, 'completed')",
            params![r#"{"urgency":"high","sentiment":"neutral","confidence":"low"}"#, cid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status)
             VALUES ('h2', 'v', 'm', ?1, 'ticket_analysis', ?2, 'completed')",
            params![r#"{"urgency":"normal","sentiment":"neutral","confidence":"low"}"#, cid],
        )
        .unwrap();
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("ai_escalation").unwrap(), 0);
        // A newer qualifying run wins over the stale non-qualifying one.
        conn.execute(
            "INSERT INTO ai_runs (input_hash, prompt_version, model, response_json, type, conversation_id, status)
             VALUES ('h3', 'v', 'm', ?1, 'ticket_analysis', ?2, 'completed')",
            params![r#"{"urgency":"critical","sentiment":"neutral","confidence":"high"}"#, cid],
        )
        .unwrap();
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("ai_escalation").unwrap(), 1);
    }

    // ---- SLA tiles (ported from the T16 suite) -----------------------------------

    #[test]
    fn sla_tiles_count_from_the_business_minutes_engine() {
        let conn = fresh_db();
        seed_sla_fixture(&conn, 120, 50);
        let snap = snapshot(&conn, None).unwrap();
        assert_eq!(snap.count_of("sla_breached").unwrap(), 1);
        assert_eq!(snap.count_of("sla_at_risk").unwrap(), 1);
        // Scoped to a mailbox with no alerts: 0, honestly.
        let scoped = snapshot(&conn, Some(&[999])).unwrap();
        assert_eq!(scoped.count_of("sla_breached").unwrap(), 0);
        assert_eq!(scoped.count_of("sla_at_risk").unwrap(), 0);
    }

    #[test]
    fn sla_tiles_are_mailbox_scoped_in_the_snapshot() {
        let conn = fresh_db();
        seed_sla_fixture(&conn, 120, 50);
        let scoped = snapshot(&conn, Some(&[101])).unwrap();
        assert_eq!(scoped.count_of("sla_breached").unwrap(), 1);
        assert_eq!(scoped.count_of("sla_at_risk").unwrap(), 1);
        let other = snapshot(&conn, Some(&[102])).unwrap();
        assert_eq!(other.count_of("sla_breached").unwrap(), 0);
        assert_eq!(other.count_of("sla_at_risk").unwrap(), 0);
    }

    #[test]
    fn sla_tile_note_names_unconfigured_mailboxes() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (101, 201, 'Support')",
            [],
        )
        .unwrap();
        insert_conversation(&conn, 4001, "active", 101, None, None);
        let snap = snapshot(&conn, None).unwrap();
        let tile = snap.tiles.iter().find(|t| t.key == "sla_at_risk").unwrap();
        assert_eq!(
            tile.note.as_deref(),
            Some("1 mailbox(es) have no business hours/targets configured and are not monitored.")
        );
    }
}
