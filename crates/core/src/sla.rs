//! SLA reporting service — the port of `src/server/analytics/slaService.ts`
//! (v1.4.0 report / v1.5.0 alerts): per-mailbox first-response and resolution
//! performance measured in BUSINESS minutes against configurable schedules
//! and targets, plus live "currently waiting" aging for breach risk.
//!
//! Design decisions (mirrored from the reference):
//! - SQL selects the (created, first-reply / closed) pairs; the
//!   business-hours arithmetic happens in Rust because timezone/DST math
//!   belongs to the tz database, not to hand-rolled SQL. The row counts are
//!   bounded by the report window, so this stays fast on a local mirror.
//! - Metric honesty: every row carries both wall and business minutes, and a
//!   `business_hours_configured` flag. Without a configured schedule the
//!   report still works - in wall minutes, labeled as such. Nothing silently
//!   pretends to be business-hours-adjusted.
//! - SLA alerts measure BUSINESS minutes since each conversation's last
//!   CUSTOMER message (a conversation merely awaiting the customer is not
//!   breaching anything), against the mailbox's first-response target (no
//!   agent reply yet) or resolution target (replied but unresolved).
//!   Mailboxes without business hours or without targets are reported as
//!   `unconfigured`, never guessed.
//!
//! Port mapping notes (port column <- reference column):
//! - since M047 the `conversations` table carries the reference names
//!   `mailbox_local_id`/`assignee_local_id` directly (the former port-side
//!   `mailbox_id`/`assignee_id` renames are gone)
//! - `conversations.created_at` <- `remote_created_at`
//! - `conversations.updated_at` <- `last_activity_at` (the search module
//!   established this mapping)
//! - `conversation_threads` <- `threads` (`thread_type`/`created_at` instead
//!   of `type`/`remote_created_at`; `state` exists via M030 with the same
//!   'published' default)
//! - `mailboxes` has no `deleted_at` in the port's mirror, so the mailbox
//!   listing drops that filter (same shape as the settings business-hours
//!   route).
//! - Window filters keep the reference's LEXICAL ISO-8601 string comparison
//!   (from/to arrive as `toISOString()` strings), quirks included.

use crate::business_hours::{
    business_minutes_between, default_business_hours, is_valid_timezone, now_iso_millis,
    sla_status, wall_minutes_between, BusinessHoursConfig, SlaTargets, SlaVerdict,
};
use crate::error::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// A conversation enters at-risk once it has consumed >= 80% of its target
/// (reference `SlaService.AT_RISK_RATIO`, v1.5.0).
const AT_RISK_RATIO: f64 = 0.8;

/// The alerts list is capped at this many rows (reference `alerts.slice(0, 50)`).
const ALERTS_CAP: usize = 50;

/// The reference's fixed `note` string on the alerts payload.
const ALERTS_NOTE: &str = "Alerts measure BUSINESS minutes (nights/weekends excluded) since each conversation's last customer message, against the mailbox's SLA targets. Mailboxes without business hours or targets are listed as unconfigured - nothing is guessed.";

// ---------------------------------------------------------------------------
// Response shapes (reference slaService.ts interfaces, field-for-field)
// ---------------------------------------------------------------------------

/// Reference `SlaDurationStats`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaDurationStats {
    /// All pairs found in the window (including unmeasurable ones).
    pub count: i64,
    pub avg_wall_min: Option<i64>,
    pub avg_business_min: Option<i64>,
    pub median_business_min: Option<i64>,
    pub met: i64,
    pub missed: i64,
    pub no_target: i64,
    pub target_min: Option<i64>,
}

/// Reference `SlaWaitingStats`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaWaitingStats {
    pub count: i64,
    pub oldest_business_min: Option<i64>,
    pub avg_business_min: Option<i64>,
    pub at_risk: i64,
}

/// The schedule block of a report row (reference inline type — camelCase
/// JSON keys like the reference's `startMinute`/`endMinute`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlaSchedule {
    pub timezone: String,
    pub days: Vec<i64>,
    pub start_minute: i64,
    pub end_minute: i64,
}

/// Reference `SlaMailboxRow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaMailboxRow {
    pub mailbox_id: i64,
    pub mailbox_name: String,
    pub business_hours_configured: bool,
    pub schedule: Option<SlaSchedule>,
    pub conversations_in_range: i64,
    pub first_response: SlaDurationStats,
    pub resolution: SlaDurationStats,
    pub waiting: SlaWaitingStats,
}

/// Reference `SlaReport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaReport {
    pub range: SlaRange,
    pub mailboxes: Vec<SlaMailboxRow>,
    pub unconfigured_mailboxes: Vec<String>,
    pub source: Vec<String>,
}

/// The `range` block of [`SlaReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaRange {
    pub from: String,
    pub to: String,
}

/// Reference `SlaAlertRow`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaAlertRow {
    pub conversation_id: i64,
    pub number: i64,
    pub subject: Option<String>,
    pub status: String,
    pub mailbox_id: i64,
    pub mailbox_name: String,
    pub assignee_local_id: Option<i64>,
    pub state: SlaAlertState,
    pub waited_business_min: i64,
    pub target_min: i64,
    pub target_kind: SlaTargetKind,
    pub overdue_business_min: i64,
    pub since: String,
}

/// The alert state (reference union 'breached' | 'at_risk').
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlaAlertState {
    /// Business minutes past the target.
    Breached,
    /// Business minutes at 80%+ of the target (not yet past it).
    AtRisk,
}

impl SlaAlertState {
    /// The state as the tile filter sees it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Breached => "breached",
            Self::AtRisk => "at_risk",
        }
    }
}

/// The target an alert measured against (reference union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlaTargetKind {
    /// No agent reply yet.
    FirstResponse,
    /// Replied but unresolved.
    Resolution,
}

impl SlaTargetKind {
    /// The kind as the route payload carries it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstResponse => "first_response",
            Self::Resolution => "resolution",
        }
    }
}

/// One `per_mailbox` rollup row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaPerMailbox {
    pub mailbox_id: i64,
    pub mailbox_name: String,
    pub breached: i64,
    pub at_risk: i64,
    pub monitored: i64,
}

/// Reference `SlaAlerts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaAlerts {
    pub generated_at: String,
    pub total_breached: i64,
    pub total_at_risk: i64,
    pub alerts: Vec<SlaAlertRow>,
    pub per_mailbox: Vec<SlaPerMailbox>,
    pub unconfigured_mailboxes: Vec<String>,
    pub note: &'static str,
}

/// The stored schedule + targets for one mailbox (the reference's
/// `getEffectiveConfig` return: `BusinessHoursConfig & SlaTargets`).
#[derive(Debug, Clone, PartialEq)]
struct EffectiveConfig {
    business_hours: BusinessHoursConfig,
    targets: SlaTargets,
}

/// A (start, end) duration pair measured off the mirror.
#[derive(Debug, Clone)]
struct Pair {
    start: Option<String>,
    end: Option<String>,
}

/// Ensure the tables/columns the engine reads exist (idempotent, the port's
/// lazy-guard pattern — the reference gets this schema from migration 008 +
/// 001, the canonical boot chain creates it, and this keeps the engine's SQL
/// valid on every database, e.g. the operations test harness which boots a
/// narrower migration chain). Both engine entry points call it; tests that
/// seed `mailbox_business_hours` directly call it first.
pub fn ensure_sla_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mailbox_business_hours (
            mailbox_local_id INTEGER PRIMARY KEY,
            timezone TEXT NOT NULL DEFAULT 'UTC',
            days TEXT NOT NULL DEFAULT '[1,2,3,4,5]',
            start_minute INTEGER NOT NULL DEFAULT 540,
            end_minute INTEGER NOT NULL DEFAULT 1020,
            first_response_target_min INTEGER,
            resolution_target_min INTEGER,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS conversation_threads (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL,
            type            TEXT NOT NULL,
            body_text       TEXT,
            from_type       TEXT,
            created_by_customer_id INTEGER,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );",
    )?;
    ensure_column(
        conn,
        "conversations",
        "deleted_at",
        "ALTER TABLE conversations ADD COLUMN deleted_at TEXT",
    )?;
    ensure_column(
        conn,
        "conversations",
        "snoozed_until",
        "ALTER TABLE conversations ADD COLUMN snoozed_until TEXT",
    )?;
    ensure_column(
        conn,
        "conversation_threads",
        "deleted_at",
        "ALTER TABLE conversation_threads ADD COLUMN deleted_at TEXT",
    )?;
    ensure_column(
        conn,
        "conversation_threads",
        "state",
        "ALTER TABLE conversation_threads ADD COLUMN state TEXT DEFAULT 'published'",
    )?;
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` only when the column is missing.
fn ensure_column(conn: &Connection, table: &str, column: &str, ddl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .flatten()
        .collect();
    if !existing.iter().any(|c| c == column) {
        conn.execute_batch(ddl)?;
    }
    Ok(())
}

/// Reference `sanitizeIds`: trunc, keep positive integers. An empty result
/// means "no filter" (all mailboxes).
fn sanitize_ids(ids: Option<&[i64]>) -> Vec<i64> {
    ids.unwrap_or_default()
        .iter()
        .copied()
        .filter(|id| *id > 0)
        .collect()
}

/// The raw `mailbox_business_hours` row (columns in table order).
struct StoredBusinessHours {
    timezone: String,
    days: String,
    start_minute: i64,
    end_minute: i64,
    first_response_target_min: Option<i64>,
    resolution_target_min: Option<i64>,
}

/// Reference `getEffectiveConfig`: stored row -> schedule + targets.
/// Invalid timezones degrade to UTC; unparsable/empty `days` degrade to the
/// Mon-Fri default (the engine guards both again).
fn effective_config(conn: &Connection, mailbox_local_id: i64) -> Result<Option<EffectiveConfig>> {
    let stored: Option<StoredBusinessHours> = conn
        .query_row(
            "SELECT timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min
             FROM mailbox_business_hours WHERE mailbox_local_id = ?1",
            [mailbox_local_id],
            |r| {
                Ok(StoredBusinessHours {
                    timezone: r.get(0)?,
                    days: r.get(1)?,
                    start_minute: r.get(2)?,
                    end_minute: r.get(3)?,
                    first_response_target_min: r.get(4)?,
                    resolution_target_min: r.get(5)?,
                })
            },
        )
        .optional()?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let mut days = default_business_hours().days;
    if let Ok(parsed) = serde_json::from_str::<Vec<i64>>(&stored.days) {
        if !parsed.is_empty() {
            days = parsed;
        }
    }
    Ok(Some(EffectiveConfig {
        business_hours: BusinessHoursConfig {
            timezone: if is_valid_timezone(&stored.timezone) {
                stored.timezone
            } else {
                "UTC".to_string()
            },
            days,
            start_minute: stored.start_minute,
            end_minute: stored.end_minute,
        },
        targets: SlaTargets {
            first_response_target_min: stored.first_response_target_min,
            resolution_target_min: stored.resolution_target_min,
        },
    }))
}

/// Per-mailbox SLA report over the window (reference `slaReport`).
///
/// `from`/`to` are used verbatim, exactly like the reference: they bound the
/// window via lexical ISO comparison and round-trip into `range`.
pub fn sla_report(
    conn: &Connection,
    from: &str,
    to: &str,
    mailbox_local_ids: Option<&[i64]>,
) -> Result<SlaReport> {
    ensure_sla_schema(conn)?;
    let id_filter = sanitize_ids(mailbox_local_ids);

    // SELECT id, name FROM mailboxes WHERE deleted_at IS NULL [AND id IN (...)]
    // ORDER BY name  (the port's mirror has no deleted_at on mailboxes)
    let mut mailboxes: Vec<(i64, String)> = Vec::new();
    {
        let sql = if id_filter.is_empty() {
            "SELECT id, name FROM mailboxes ORDER BY name".to_string()
        } else {
            format!(
                "SELECT id, name FROM mailboxes WHERE id IN ({}) ORDER BY name",
                id_filter
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let params: Vec<&dyn rusqlite::ToSql> = id_filter
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            mailboxes.push(row?);
        }
    }

    let mut rows: Vec<SlaMailboxRow> = Vec::new();
    let mut unconfigured: Vec<String> = Vec::new();
    for (id, name) in mailboxes {
        let cfg = effective_config(conn, id)?;
        if cfg.is_none() {
            unconfigured.push(name.clone());
        }
        let schedule = cfg.as_ref().map(|c| SlaSchedule {
            timezone: c.business_hours.timezone.clone(),
            days: c.business_hours.days.clone(),
            start_minute: c.business_hours.start_minute,
            end_minute: c.business_hours.end_minute,
        });
        rows.push(SlaMailboxRow {
            mailbox_id: id,
            mailbox_name: name,
            business_hours_configured: cfg.is_some(),
            schedule,
            conversations_in_range: count_conversations(conn, id, from, to),
            first_response: duration_stats(
                conn,
                id,
                from,
                to,
                DurationKind::FirstResponse,
                cfg.as_ref(),
            ),
            resolution: duration_stats(conn, id, from, to, DurationKind::Resolution, cfg.as_ref()),
            waiting: waiting_stats(conn, id, cfg.as_ref())?,
        });
    }
    Ok(SlaReport {
        range: SlaRange {
            from: from.to_string(),
            to: to.to_string(),
        },
        mailboxes: rows,
        unconfigured_mailboxes: unconfigured,
        source: vec!["local".to_string()],
    })
}

/// Reference `countConversations` (created inside the window).
fn count_conversations(conn: &Connection, mailbox_local_id: i64, from: &str, to: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM conversations
         WHERE mailbox_local_id = ?1 AND deleted_at IS NULL AND created_at >= ?2 AND created_at <= ?3",
        rusqlite::params![mailbox_local_id, from, to],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Which duration pairs to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationKind {
    FirstResponse,
    Resolution,
}

/// Reference `firstResponsePairs`: conversation created -> first published
/// reply.
fn first_response_pairs(
    conn: &Connection,
    mailbox_local_id: i64,
    from: &str,
    to: &str,
) -> Result<Vec<Pair>> {
    let mut stmt = conn.prepare(
        "SELECT c.created_at AS start, fr.first_reply AS end
         FROM conversations c
         JOIN (SELECT conversation_id, MIN(created_at) AS first_reply
               FROM conversation_threads
               WHERE type = 'reply' AND state = 'published' AND deleted_at IS NULL
               GROUP BY conversation_id) fr
           ON fr.conversation_id = c.id
         WHERE c.mailbox_local_id = ?1 AND c.deleted_at IS NULL AND c.created_at >= ?2 AND c.created_at <= ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![mailbox_local_id, from, to], |r| {
        Ok(Pair {
            start: r.get(0)?,
            end: r.get(1)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

/// Reference `resolutionPairs`: conversation created -> closed.
fn resolution_pairs(
    conn: &Connection,
    mailbox_local_id: i64,
    from: &str,
    to: &str,
) -> Result<Vec<Pair>> {
    let mut stmt = conn.prepare(
        "SELECT c.created_at AS start, c.closed_at AS end
         FROM conversations c
         WHERE c.mailbox_local_id = ?1 AND c.deleted_at IS NULL AND c.closed_at IS NOT NULL
           AND c.closed_at >= ?2 AND c.closed_at <= ?3",
    )?;
    let rows = stmt.query_map(rusqlite::params![mailbox_local_id, from, to], |r| {
        Ok(Pair {
            start: r.get(0)?,
            end: r.get(1)?,
        })
    })?;
    Ok(rows.flatten().collect())
}

/// JS `Math.round` for non-negative values (round half up, like the
/// reference's averages/medians).
fn js_round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// Reference `durationStats`: measured pairs -> wall/business stats + the
/// met/missed verdicts. The verdict uses business minutes when available,
/// wall minutes otherwise; `count` counts ALL pairs; without a target every
/// pair is `no_target`.
fn duration_stats(
    conn: &Connection,
    mailbox_local_id: i64,
    from: &str,
    to: &str,
    kind: DurationKind,
    cfg: Option<&EffectiveConfig>,
) -> SlaDurationStats {
    let pairs = match kind {
        DurationKind::FirstResponse => {
            first_response_pairs(conn, mailbox_local_id, from, to).unwrap_or_default()
        }
        DurationKind::Resolution => {
            resolution_pairs(conn, mailbox_local_id, from, to).unwrap_or_default()
        }
    };
    let target = match kind {
        DurationKind::FirstResponse => cfg.and_then(|c| c.targets.first_response_target_min),
        DurationKind::Resolution => cfg.and_then(|c| c.targets.resolution_target_min),
    };
    let mut walls: Vec<f64> = Vec::new();
    let mut businesses: Vec<f64> = Vec::new();
    let (mut met, mut missed, mut no_target) = (0i64, 0i64, 0i64);
    for p in &pairs {
        let (Some(start), Some(end)) = (p.start.as_deref(), p.end.as_deref()) else {
            continue;
        };
        let Some(wall) = wall_minutes_between(start, end) else {
            continue;
        };
        walls.push(wall);
        let business = cfg.map(|c| business_minutes_between(start, end, &c.business_hours));
        if let Some(Some(b)) = business {
            businesses.push(b);
        }
        match sla_status(business.flatten(), target) {
            SlaVerdict::Met => met += 1,
            SlaVerdict::Missed => missed += 1,
            _ => no_target += 1,
        }
    }
    SlaDurationStats {
        count: pairs.len() as i64,
        avg_wall_min: avg_rounded(&walls),
        avg_business_min: if businesses.is_empty() {
            None
        } else {
            avg_rounded(&businesses)
        },
        median_business_min: if businesses.is_empty() {
            None
        } else {
            Some(median_rounded(&businesses))
        },
        met,
        missed,
        no_target: if target.is_none() {
            pairs.len() as i64
        } else {
            no_target
        },
        target_min: target,
    }
}

/// Reference `avg`: rounded mean, `None` for an empty list.
fn avg_rounded(xs: &[f64]) -> Option<i64> {
    if xs.is_empty() {
        return None;
    }
    Some(js_round(xs.iter().sum::<f64>() / xs.len() as f64))
}

/// Reference `median`: rounded median (mean of the two middles for even n).
fn median_rounded(xs: &[f64]) -> i64 {
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        js_round(sorted[mid])
    } else {
        js_round((sorted[mid - 1] + sorted[mid]) / 2.0)
    }
}

/// Reference `waitingStats`: currently-open conversations aging since their
/// last activity. Without a schedule these age in WALL minutes (the report's
/// honest fallback); `at_risk` counts those strictly past the first-response
/// target.
fn waiting_stats(
    conn: &Connection,
    mailbox_local_id: i64,
    cfg: Option<&EffectiveConfig>,
) -> Result<SlaWaitingStats> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(c.updated_at, c.created_at) AS since
         FROM conversations c
         WHERE c.mailbox_local_id = ?1 AND c.status IN ('active','pending') AND c.deleted_at IS NULL",
    )?;
    let rows = stmt.query_map([mailbox_local_id], |r| r.get::<_, Option<String>>(0))?;
    let mut sinces: Vec<Option<String>> = Vec::new();
    for row in rows {
        sinces.push(row?);
    }
    let now = now_iso_millis();
    let mut businesses: Vec<f64> = Vec::new();
    for since in sinces.iter().flatten() {
        let b = match cfg {
            Some(c) => business_minutes_between(since, &now, &c.business_hours),
            None => wall_minutes_between(since, &now),
        };
        if let Some(b) = b {
            businesses.push(b);
        }
    }
    let target = cfg.and_then(|c| c.targets.first_response_target_min);
    let at_risk = match target {
        Some(t) => businesses.iter().filter(|b| **b > t as f64).count() as i64,
        None => 0,
    };
    Ok(SlaWaitingStats {
        count: sinces.len() as i64,
        oldest_business_min: businesses
            .iter()
            .copied()
            .fold(None, |a: Option<f64>, b| {
                Some(match a {
                    Some(x) => x.max(b),
                    None => b,
                })
            })
            .map(js_round),
        avg_business_min: avg_rounded(&businesses),
        at_risk,
    })
}

/// One candidate row of the alerts scan.
#[derive(Debug, Clone)]
struct AlertCandidate {
    id: i64,
    number: i64,
    subject: Option<String>,
    status: String,
    assignee_local_id: Option<i64>,
    since: Option<String>,
    replied: bool,
    customer_threads: i64,
}

/// Business-hours-aware SLA alerts for the Issue Radar (reference
/// `slaAlerts`, v1.5.0).
///
/// For every ACTIVE/PENDING conversation with a business-hours-configured
/// mailbox we measure how long it has been waiting (business minutes since
/// the last CUSTOMER message) and compare against the mailbox's
/// first-response target (no agent reply yet) or resolution target (replied
/// but unresolved). Snoozed conversations are skipped. Honest states
/// everywhere: mailboxes without business hours or without targets are
/// reported as `unconfigured`, never guessed.
pub fn sla_alerts(conn: &Connection) -> Result<SlaAlerts> {
    ensure_sla_schema(conn)?;
    let now = now_iso_millis();

    let mut mailboxes: Vec<(i64, String)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, name FROM mailboxes ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            mailboxes.push(row?);
        }
    }

    let mut alerts: Vec<SlaAlertRow> = Vec::new();
    let mut per_mailbox: Vec<SlaPerMailbox> = Vec::new();
    let mut unconfigured: Vec<String> = Vec::new();

    for (id, name) in mailboxes {
        let Some(cfg) = effective_config(conn, id)? else {
            let open: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM conversations
                     WHERE mailbox_local_id = ?1 AND status IN ('active','pending') AND deleted_at IS NULL",
                    [id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if open > 0 {
                unconfigured.push(name.clone());
            }
            per_mailbox.push(SlaPerMailbox {
                mailbox_id: id,
                mailbox_name: name,
                breached: 0,
                at_risk: 0,
                monitored: 0,
            });
            continue;
        };

        // Waiting conversations + whether an agent reply exists.
        let candidates: Vec<AlertCandidate> = {
            let mut stmt = conn.prepare(
                "SELECT c.id, c.number, c.subject, c.status, c.assignee_local_id,
                        COALESCE(c.updated_at, c.created_at) AS since,
                        EXISTS (SELECT 1 FROM conversation_threads t
                                WHERE t.conversation_id = c.id AND t.type = 'reply'
                                  AND t.state = 'published' AND t.deleted_at IS NULL) AS replied,
                        (SELECT COUNT(*) FROM conversation_threads t
                         WHERE t.conversation_id = c.id AND t.type = 'customer'
                           AND t.deleted_at IS NULL) AS customer_threads
                 FROM conversations c
                 WHERE c.mailbox_local_id = ?1 AND c.status IN ('active','pending') AND c.deleted_at IS NULL
                   AND NOT (c.snoozed_until IS NOT NULL AND c.snoozed_until > ?2)",
            )?;
            let rows = stmt.query_map(rusqlite::params![id, now], |r| {
                Ok(AlertCandidate {
                    id: r.get(0)?,
                    number: r.get(1)?,
                    subject: r.get(2)?,
                    status: r.get(3)?,
                    assignee_local_id: r.get(4)?,
                    since: r.get(5)?,
                    replied: r.get::<_, i64>(6)? != 0,
                    customer_threads: r.get(7)?,
                })
            })?;
            rows.flatten().collect()
        };

        let mut breached = 0i64;
        let mut at_risk = 0i64;
        for r in &candidates {
            if r.since.is_none() || r.customer_threads == 0 {
                continue; // nothing awaiting us
            }
            // The clock starts at the last CUSTOMER message, not the last activity
            let last_customer: Option<String> = conn
                .query_row(
                    "SELECT created_at FROM conversation_threads
                     WHERE conversation_id = ?1 AND type = 'customer' AND deleted_at IS NULL
                     ORDER BY created_at DESC LIMIT 1",
                    [r.id],
                    |row| row.get(0),
                )
                .optional()?;
            let last_customer =
                last_customer.unwrap_or_else(|| r.since.clone().unwrap_or_default());
            let target = if r.replied {
                cfg.targets.resolution_target_min
            } else {
                cfg.targets.first_response_target_min
            };
            let Some(target) = target else {
                continue;
            };
            let Some(waited) = business_minutes_between(&last_customer, &now, &cfg.business_hours)
            else {
                continue;
            };
            let state = if waited > target as f64 {
                SlaAlertState::Breached
            } else if waited >= target as f64 * AT_RISK_RATIO {
                SlaAlertState::AtRisk
            } else {
                continue; // 'ok' — not alerted
            };
            match state {
                SlaAlertState::Breached => breached += 1,
                SlaAlertState::AtRisk => at_risk += 1,
            }
            alerts.push(SlaAlertRow {
                conversation_id: r.id,
                number: r.number,
                subject: r.subject.clone(),
                status: r.status.clone(),
                mailbox_id: id,
                mailbox_name: name.clone(),
                assignee_local_id: r.assignee_local_id,
                state,
                waited_business_min: js_round(waited),
                target_min: target,
                target_kind: if r.replied {
                    SlaTargetKind::Resolution
                } else {
                    SlaTargetKind::FirstResponse
                },
                overdue_business_min: js_round((waited - target as f64).max(0.0)),
                since: last_customer,
            });
        }
        per_mailbox.push(SlaPerMailbox {
            mailbox_id: id,
            mailbox_name: name,
            breached,
            at_risk,
            monitored: candidates.len() as i64,
        });
    }

    // overdue DESC, then number ASC (reference sort)
    alerts.sort_by(|a, b| {
        b.overdue_business_min
            .cmp(&a.overdue_business_min)
            .then_with(|| a.number.cmp(&b.number))
    });
    let total_breached = alerts
        .iter()
        .filter(|a| a.state == SlaAlertState::Breached)
        .count() as i64;
    let total_at_risk = alerts
        .iter()
        .filter(|a| a.state == SlaAlertState::AtRisk)
        .count() as i64;
    alerts.truncate(ALERTS_CAP);
    Ok(SlaAlerts {
        generated_at: now,
        total_breached,
        total_at_risk,
        alerts,
        per_mailbox,
        unconfigured_mailboxes: unconfigured,
        note: ALERTS_NOTE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rusqlite::params;

    /// Fresh DB with the full migration chain, plus one mailbox (id 1) and
    /// helpers to seed conversations/threads.
    fn fresh_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
            [],
        )
        .unwrap();
        conn
    }

    fn seed_conversation(conn: &Connection, remote_id: i64, status: &str, created_at: &str) -> i64 {
        // M047 conversations FK: customer_local_id -> customers(id).
        conn.execute(
            "INSERT OR IGNORE INTO customers (id, remote_id, first_name) VALUES (3001, 3001, 'Cust')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, mailbox_local_id, customer_local_id, status, created_at, updated_at, closed_at)
             VALUES (?1, ?1, 1, 3001, ?2, ?3, ?3, ?4)",
            params![remote_id, status, created_at, if status == "closed" { Some(created_at) } else { None }],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM conversations WHERE remote_id = ?1",
            [remote_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn seed_thread(conn: &Connection, conv_id: i64, kind: &str, at: &str) {
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, type, state, body_text, from_type, created_at)
             VALUES (?1, ?2, 'published', 'body', 'customer', ?3)",
            params![conv_id, kind, at],
        )
        .unwrap();
    }

    /// 24/7 UTC schedule with a 60-minute first-response target and a
    /// 480-minute resolution target (the shape the reference e2e suite
    /// configures).
    fn configure_247(conn: &Connection, fr: Option<i64>, res: Option<i64>) {
        conn.execute(
            "INSERT INTO mailbox_business_hours
                (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
             VALUES (1, 'UTC', '[0,1,2,3,4,5,6]', 0, 1440, ?1, ?2, '2026-01-01T00:00:00.000Z')",
            params![fr, res],
        )
        .unwrap();
    }

    fn minutes_ago(mins: i64) -> String {
        let ts = Utc::now().timestamp_millis() - mins * 60_000;
        Utc.timestamp_millis_opt(ts)
            .single()
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    // ---- slaReport golden cases --------------------------------------------

    /// Reference e2e "reports wall-clock honestly before any schedule is
    /// configured": no business minutes, wall minutes present, every pair
    /// `no_target`, and the mailbox listed as unconfigured.
    #[test]
    fn report_wall_clock_before_configuration() {
        let conn = fresh_db();
        let c1 = seed_conversation(&conn, 101, "closed", "2026-03-02T09:00:00Z");
        seed_thread(&conn, c1, "customer", "2026-03-02T09:00:00Z");
        seed_thread(&conn, c1, "reply", "2026-03-02T10:00:00Z");

        let report = sla_report(
            &conn,
            "2026-03-01T00:00:00.000Z",
            "2026-03-31T00:00:00.000Z",
            None,
        )
        .unwrap();
        assert_eq!(report.source, vec!["local".to_string()]);
        assert_eq!(report.unconfigured_mailboxes, vec!["Support"]);
        assert_eq!(report.mailboxes.len(), 1);
        let m = &report.mailboxes[0];
        assert_eq!(m.mailbox_name, "Support");
        assert!(!m.business_hours_configured);
        assert!(m.schedule.is_none());
        assert_eq!(m.conversations_in_range, 1);
        let fr = &m.first_response;
        assert_eq!(fr.count, 1);
        assert_eq!(fr.avg_wall_min, Some(60)); // 09:00 -> 10:00
        assert_eq!(fr.avg_business_min, None); // no schedule configured
        assert_eq!(fr.median_business_min, None);
        assert_eq!(fr.target_min, None);
        assert_eq!(fr.no_target, 1, "no target -> every pair is no_target");
        assert_eq!(fr.met, 0);
        assert_eq!(fr.missed, 0);
        // Resolution pair: created -> closed (same instant in this fixture).
        assert_eq!(m.resolution.count, 1);
        assert_eq!(m.resolution.avg_wall_min, Some(0));
    }

    /// Reference e2e "after configuration the report measures business
    /// minutes and classifies SLA": 24/7 schedule -> business == wall,
    /// target_min surfaces, met/missed classify.
    #[test]
    fn report_after_configuration_measures_business_minutes() {
        let conn = fresh_db();
        configure_247(&conn, Some(60), Some(4320));
        // A met pair (30 min) and a missed pair (120 min) — both closed.
        let c1 = seed_conversation(&conn, 101, "closed", "2026-03-02T09:00:00Z");
        seed_thread(&conn, c1, "customer", "2026-03-02T09:00:00Z");
        seed_thread(&conn, c1, "reply", "2026-03-02T09:30:00Z");
        let c2 = seed_conversation(&conn, 102, "closed", "2026-03-03T09:00:00Z");
        seed_thread(&conn, c2, "customer", "2026-03-03T09:00:00Z");
        seed_thread(&conn, c2, "reply", "2026-03-03T11:00:00Z");

        let report = sla_report(
            &conn,
            "2026-03-01T00:00:00.000Z",
            "2026-03-31T00:00:00.000Z",
            None,
        )
        .unwrap();
        assert!(report.unconfigured_mailboxes.is_empty());
        let m = &report.mailboxes[0];
        assert!(m.business_hours_configured);
        let schedule = m.schedule.as_ref().unwrap();
        assert_eq!(schedule.timezone, "UTC");
        assert_eq!(schedule.days, vec![0, 1, 2, 3, 4, 5, 6]);
        let fr = &m.first_response;
        assert_eq!(fr.count, 2);
        assert_eq!(
            fr.avg_business_min, fr.avg_wall_min,
            "24/7: business == wall"
        );
        assert_eq!(fr.avg_business_min, Some(75)); // (30 + 120) / 2
        assert_eq!(fr.median_business_min, Some(75));
        assert_eq!(fr.target_min, Some(60));
        assert_eq!(fr.met, 1);
        assert_eq!(fr.missed, 1);
        assert_eq!(fr.no_target, 0);
    }

    /// Business-hours schedule actually excludes nights/weekends in the
    /// report: a Friday-17:05 -> Monday-09:05 first reply measures 5
    /// business minutes (the engine's canonical case, through the service).
    #[test]
    fn report_measures_business_not_wall_minutes() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO mailbox_business_hours
                (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
             VALUES (1, 'UTC', '[1,2,3,4,5]', 540, 1020, 60, 4320, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        // Friday Mar 6 17:05Z -> Monday Mar 9 09:05Z.
        let c1 = seed_conversation(&conn, 101, "closed", "2026-03-06T17:05:00Z");
        seed_thread(&conn, c1, "customer", "2026-03-06T17:05:00Z");
        seed_thread(&conn, c1, "reply", "2026-03-09T09:05:00Z");

        let report = sla_report(
            &conn,
            "2026-03-01T00:00:00.000Z",
            "2026-03-31T00:00:00.000Z",
            None,
        )
        .unwrap();
        let fr = &report.mailboxes[0].first_response;
        assert_eq!(fr.count, 1);
        assert_eq!(fr.avg_wall_min, Some(3840)); // Fri 17:05 -> Mon 09:05 wall
        assert_eq!(fr.avg_business_min, Some(5));
        // 5 <= 60 -> met in business minutes even though wall minutes missed.
        assert_eq!(fr.met, 1);
        assert_eq!(fr.missed, 0);
    }

    /// The waiting block ages open conversations; without a schedule it uses
    /// wall minutes, and at_risk counts those past the first-response target.
    #[test]
    fn report_waiting_stats_and_at_risk() {
        let conn = fresh_db();
        // No schedule: wall minutes, target null -> at_risk 0.
        let c1 = seed_conversation(&conn, 101, "active", &minutes_ago(300));
        seed_thread(&conn, c1, "customer", &minutes_ago(300));
        let c2 = seed_conversation(&conn, 102, "pending", &minutes_ago(20));
        let _ = c2;
        let report = sla_report(
            &conn,
            "2000-01-01T00:00:00.000Z",
            "2999-01-01T00:00:00.000Z",
            None,
        )
        .unwrap();
        let w = &report.mailboxes[0].waiting;
        assert_eq!(w.count, 2);
        assert_eq!(w.at_risk, 0, "no target -> at_risk stays 0");
        let oldest = w.oldest_business_min.unwrap();
        assert!(
            (299..=301).contains(&oldest),
            "oldest wall minutes: {oldest}"
        );

        // Now with a 24/7 schedule + 60-minute target: the 300-minute-old
        // conversation is at risk, the 20-minute one is not.
        conn.execute("DELETE FROM conversations", []).unwrap();
        configure_247(&conn, Some(60), Some(480));
        let c1 = seed_conversation(&conn, 201, "active", &minutes_ago(300));
        seed_thread(&conn, c1, "customer", &minutes_ago(300));
        let c2 = seed_conversation(&conn, 202, "pending", &minutes_ago(20));
        let _ = c2;
        let report = sla_report(
            &conn,
            "2000-01-01T00:00:00.000Z",
            "2999-01-01T00:00:00.000Z",
            None,
        )
        .unwrap();
        let w = &report.mailboxes[0].waiting;
        assert_eq!(w.count, 2);
        assert_eq!(w.at_risk, 1);
        // avg over [300, 20] business minutes (24/7: business == wall)
        let avg = w.avg_business_min.unwrap();
        assert!((159..=161).contains(&avg), "avg: {avg}");
    }

    /// mailboxIds filter: only the listed mailbox is reported.
    #[test]
    fn report_scopes_to_requested_mailboxes() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (2, 202, 'Billing')",
            [],
        )
        .unwrap();
        let report = sla_report(
            &conn,
            "2000-01-01T00:00:00.000Z",
            "2999-01-01T00:00:00.000Z",
            Some(&[2]),
        )
        .unwrap();
        assert_eq!(report.mailboxes.len(), 1);
        assert_eq!(report.mailboxes[0].mailbox_id, 2);
        // An all-invalid id list filters to nothing (reference: empty IN ->
        // no filter; sanitizeIds keeps only positive ints).
        let report = sla_report(
            &conn,
            "2000-01-01T00:00:00.000Z",
            "2999-01-01T00:00:00.000Z",
            Some(&[-1]),
        )
        .unwrap();
        assert_eq!(report.mailboxes.len(), 2, "-1 sanitized away -> no filter");
    }

    /// Stored junk degrades honestly: unparsable days -> Mon-Fri default,
    /// invalid timezone -> UTC (reference getEffectiveConfig).
    #[test]
    fn config_degrades_bad_days_and_timezone() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO mailbox_business_hours
                (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
             VALUES (1, 'Mars/Olympus', 'not json', 540, 1020, 60, NULL, '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        let report = sla_report(
            &conn,
            "2000-01-01T00:00:00.000Z",
            "2999-01-01T00:00:00.000Z",
            None,
        )
        .unwrap();
        let m = &report.mailboxes[0];
        assert!(m.business_hours_configured);
        let schedule = m.schedule.as_ref().unwrap();
        assert_eq!(schedule.timezone, "UTC", "invalid tz degrades to UTC");
        assert_eq!(
            schedule.days,
            vec![1, 2, 3, 4, 5],
            "unparsable days -> Mon-Fri"
        );
        assert_eq!(m.first_response.target_min, Some(60));
        assert_eq!(m.resolution.target_min, None);
    }

    // ---- slaAlerts golden cases ----------------------------------------------

    /// Reference e2e "honest unconfigured state": zero totals, the mailbox
    /// listed as unconfigured (it has open conversations), the note mentions
    /// BUSINESS minutes.
    #[test]
    fn alerts_honest_unconfigured_state() {
        let conn = fresh_db();
        let c1 = seed_conversation(&conn, 101, "active", &minutes_ago(300));
        seed_thread(&conn, c1, "customer", &minutes_ago(300));
        let alerts = sla_alerts(&conn).unwrap();
        assert_eq!(alerts.total_breached, 0);
        assert_eq!(alerts.total_at_risk, 0);
        assert!(alerts.alerts.is_empty());
        assert_eq!(alerts.unconfigured_mailboxes, vec!["Support"]);
        assert!(alerts.note.contains("BUSINESS minutes"));
        assert_eq!(alerts.per_mailbox.len(), 1);
        assert_eq!(
            alerts.per_mailbox[0].monitored, 0,
            "unconfigured -> not monitored"
        );
    }

    /// Reference e2e "then live breaches after configuration": 24/7 + 60-min
    /// first-response target — a conversation whose last customer message is
    /// 2h old is breached with overdue > 0; one at 50 minutes is at_risk
    /// (>= 80% of 60); one at 20 minutes is ok (absent).
    #[test]
    fn alerts_breach_and_at_risk_after_configuration() {
        let conn = fresh_db();
        configure_247(&conn, Some(60), Some(480));
        let c1 = seed_conversation(&conn, 101, "active", &minutes_ago(120));
        seed_thread(&conn, c1, "customer", &minutes_ago(120));
        let c2 = seed_conversation(&conn, 102, "active", &minutes_ago(50));
        seed_thread(&conn, c2, "customer", &minutes_ago(50));
        let c3 = seed_conversation(&conn, 103, "active", &minutes_ago(20));
        seed_thread(&conn, c3, "customer", &minutes_ago(20));

        let alerts = sla_alerts(&conn).unwrap();
        assert!(alerts.unconfigured_mailboxes.is_empty());
        assert_eq!(alerts.total_breached, 1);
        assert_eq!(alerts.total_at_risk, 1);
        assert_eq!(alerts.alerts.len(), 2);
        let breached = alerts
            .alerts
            .iter()
            .find(|a| a.state == SlaAlertState::Breached)
            .unwrap();
        assert_eq!(breached.conversation_id, c1);
        assert_eq!(breached.target_kind, SlaTargetKind::FirstResponse);
        assert_eq!(breached.target_min, 60);
        assert_eq!(breached.waited_business_min, 120);
        assert_eq!(breached.overdue_business_min, 60);
        let at_risk = alerts
            .alerts
            .iter()
            .find(|a| a.state == SlaAlertState::AtRisk)
            .unwrap();
        assert_eq!(at_risk.conversation_id, c2);
        assert_eq!(at_risk.overdue_business_min, 0);
        assert!(
            !alerts.alerts.iter().any(|a| a.conversation_id == c3),
            "ok conversations are absent"
        );
        assert_eq!(alerts.per_mailbox[0].monitored, 3);
        assert_eq!(alerts.per_mailbox[0].breached, 1);
        assert_eq!(alerts.per_mailbox[0].at_risk, 1);
    }

    /// The clock restarts at the last CUSTOMER message: a replied-but-stale
    /// conversation alerts on the RESOLUTION target, and a newer customer
    /// message resets the wait even when `since` (last activity) is older.
    #[test]
    fn alerts_clock_starts_at_last_customer_message_and_uses_resolution_target() {
        let conn = fresh_db();
        configure_247(&conn, Some(60), Some(480));
        // Replied 10h ago; the customer wrote again 90 minutes ago ->
        // resolution target (480) applies: 90 < 480 -> ok, no alert.
        let c1 = seed_conversation(&conn, 101, "active", &minutes_ago(600));
        seed_thread(&conn, c1, "customer", &minutes_ago(600));
        seed_thread(&conn, c1, "reply", &minutes_ago(590));
        seed_thread(&conn, c1, "customer", &minutes_ago(90));
        // Same shape but the last customer message is 9h old -> 540 > 480 ->
        // breached on the resolution target.
        let c2 = seed_conversation(&conn, 102, "active", &minutes_ago(600));
        seed_thread(&conn, c2, "customer", &minutes_ago(600));
        seed_thread(&conn, c2, "reply", &minutes_ago(590));
        seed_thread(&conn, c2, "customer", &minutes_ago(540));

        let alerts = sla_alerts(&conn).unwrap();
        assert!(
            !alerts.alerts.iter().any(|a| a.conversation_id == c1),
            "90 < 480 resolution target: ok"
        );
        let a = alerts
            .alerts
            .iter()
            .find(|a| a.conversation_id == c2)
            .unwrap();
        assert_eq!(a.state, SlaAlertState::Breached);
        assert_eq!(a.target_kind, SlaTargetKind::Resolution);
        assert_eq!(a.target_min, 480);
        assert_eq!(a.overdue_business_min, 60);
        assert_eq!(a.waited_business_min, 540);
    }

    /// Snoozed conversations and conversations with no customer message are
    /// skipped (nothing is awaiting us).
    #[test]
    fn alerts_skip_snoozed_and_customer_less_conversations() {
        let conn = fresh_db();
        configure_247(&conn, Some(60), Some(480));
        let c1 = seed_conversation(&conn, 101, "active", &minutes_ago(300));
        seed_thread(&conn, c1, "customer", &minutes_ago(300));
        conn.execute(
            "UPDATE conversations SET snoozed_until = ?1 WHERE id = ?2",
            params![minutes_ago(-600), c1],
        )
        .unwrap();
        let c2 = seed_conversation(&conn, 102, "active", &minutes_ago(300));
        let _ = c2; // no customer thread at all
                    // No targets configured -> mailboxes with open conversations show up
                    // as unconfigured instead of guessed alerts.
        let alerts = sla_alerts(&conn).unwrap();
        assert!(
            alerts.alerts.is_empty(),
            "snoozed + customer-less are skipped"
        );
        assert_eq!(
            alerts.per_mailbox[0].monitored, 1,
            "only the non-snoozed candidate is monitored"
        );
    }

    /// Mailboxes whose schedule exists but has no targets are not monitored
    /// (the `target == null` continue) yet stay "configured" in the report.
    #[test]
    fn alerts_without_targets_are_not_monitored() {
        let conn = fresh_db();
        configure_247(&conn, None, None);
        let c1 = seed_conversation(&conn, 101, "active", &minutes_ago(300));
        seed_thread(&conn, c1, "customer", &minutes_ago(300));
        let alerts = sla_alerts(&conn).unwrap();
        assert!(alerts.alerts.is_empty());
        assert!(
            alerts.unconfigured_mailboxes.is_empty(),
            "configured mailbox, just no targets"
        );
        assert_eq!(alerts.per_mailbox[0].monitored, 1);
        assert_eq!(alerts.per_mailbox[0].breached, 0);
        // The report still lists it as configured.
        let report = sla_report(
            &conn,
            "2000-01-01T00:00:00.000Z",
            "2999-01-01T00:00:00.000Z",
            None,
        )
        .unwrap();
        assert!(report.mailboxes[0].business_hours_configured);
        assert_eq!(
            report.mailboxes[0].first_response.no_target, 0,
            "no pairs measured"
        );
    }

    /// Sorting is overdue DESC then number ASC, totals count ALL alerts, and
    /// the list is capped at 50 rows (reference alerts.slice(0, 50)).
    #[test]
    fn alerts_sort_totals_and_cap() {
        let conn = fresh_db();
        configure_247(&conn, Some(60), Some(480));
        // 55 breached conversations with increasing overdue, numbers 101..
        for i in 0..55i64 {
            let c = seed_conversation(&conn, 101 + i, "active", &minutes_ago(100 + i));
            seed_thread(&conn, c, "customer", &minutes_ago(100 + i));
        }
        let alerts = sla_alerts(&conn).unwrap();
        assert_eq!(alerts.total_breached, 55, "totals count every alert");
        assert_eq!(alerts.alerts.len(), 50, "list capped at 50");
        // Sorted by overdue descending: the oldest wait (conversation with
        // the largest minutes_ago) comes first.
        let first = &alerts.alerts[0];
        assert_eq!(first.number, 101 + 54, "highest overdue first");
        assert!(alerts
            .alerts
            .windows(2)
            .all(|w| w[0].overdue_business_min >= w[1].overdue_business_min));
    }

    /// The reference's `slaReport` keeps `range` verbatim — including
    /// non-ISO junk, which simply yields empty windows.
    #[test]
    fn report_range_round_trips_verbatim() {
        let conn = fresh_db();
        let c1 = seed_conversation(&conn, 101, "closed", "2026-03-02T09:00:00Z");
        let _ = c1;
        let report = sla_report(&conn, "junk-from", "junk-to", None).unwrap();
        assert_eq!(report.range.from, "junk-from");
        assert_eq!(report.range.to, "junk-to");
        assert_eq!(report.mailboxes[0].conversations_in_range, 0);
        assert_eq!(report.mailboxes[0].first_response.count, 0);
    }
}
