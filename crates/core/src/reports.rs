//! Reports and quality — dashboards, report builder, effectiveness, friction,
//! support health, customer timeline, support graph (M8-T01 through M8-T07).
//!
//! Per spec M8: "Reports and quality: dashboards, report builder, post-resolution
//! QA, effectiveness, friction, support health, customer timeline, support graph."

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::catalog::{GraphNodeKind, ReportDimensionKey, ReportMetricKey};
use crate::error::Result;

/// Migrations M020–M022 combined.
pub const M020_TO_M022_SQL: &str = r#"
    -- M020: friction_scores
    CREATE TABLE IF NOT EXISTS friction_scores (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        effort_score    REAL NOT NULL,
        factors_json    TEXT,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_friction_scores_conv
        ON friction_scores (conversation_id);

    -- M021: customer_timeline
    CREATE TABLE IF NOT EXISTS customer_timeline (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        customer_id     INTEGER NOT NULL,
        event_type      TEXT NOT NULL,
        event_data_json TEXT,
        occurred_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_customer_timeline_customer
        ON customer_timeline (customer_id, occurred_at);

    -- M022: graph_nodes + graph_edges
    CREATE TABLE IF NOT EXISTS graph_nodes (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        kind            TEXT NOT NULL,
        entity_id       INTEGER,
        label           TEXT,
        properties_json TEXT,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_graph_nodes_kind
        ON graph_nodes (kind, entity_id);

    CREATE TABLE IF NOT EXISTS graph_edges (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id       INTEGER NOT NULL REFERENCES graph_nodes (id) ON DELETE CASCADE,
        target_id       INTEGER NOT NULL REFERENCES graph_nodes (id) ON DELETE CASCADE,
        edge_type       TEXT NOT NULL DEFAULT 'related',
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_graph_edges_source
        ON graph_edges (source_id, edge_type);
    CREATE INDEX IF NOT EXISTS idx_graph_edges_target
        ON graph_edges (target_id, edge_type);

    UPDATE app_state SET schema_version = 22 WHERE id = 1;
"#;

/// Apply M020–M022 migrations. Idempotent.
pub fn apply_m020_to_m022(conn: &Connection) -> Result<()> {
    conn.execute_batch(M020_TO_M022_SQL)?;
    Ok(())
}

// ─── M8-T01: Dashboards ──────────────────────────────────────────────────

/// Dashboard KPI metrics — the summary numbers shown on the main dashboard.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DashboardMetrics {
    pub total_conversations: u32,
    pub new_conversations: u32,
    pub closed_conversations: u32,
    pub active_conversations: u32,
    pub customer_waiting: u32,
    pub avg_first_response_minutes: Option<f64>,
    pub avg_resolution_minutes: Option<f64>,
    pub sla_breach_count: u32,
}

/// Get dashboard metrics. Per the reference notes: `analytics/` module handles
/// "Aggregations backing the dashboard." All timestamp comparisons use
/// `julianday()` per KNOWN PITFALLS.
///
/// `days_back` is the number of days to look back for "new" conversations
/// (default: 7 for the last week).
pub fn get_dashboard_metrics(
    conn: &Connection,
    mailbox_id: Option<i64>,
    days_back: u32,
) -> Result<DashboardMetrics> {
    let days_back = days_back.max(1) as i64;
    let mailbox_clause = mailbox_id
        .map(|mid| format!("AND mailbox_id = {mid}"))
        .unwrap_or_default();

    // Total conversations.
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM conversations WHERE 1=1 {mailbox_clause}"),
        [],
        |r| r.get(0),
    )?;

    // New conversations (created in the last `days_back` days).
    let new_conv: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations
             WHERE julianday(local_created_at) >= julianday('now', '-{days_back} days')
             {mailbox_clause}"
        ),
        [],
        |r| r.get(0),
    )?;

    // Closed conversations.
    let closed: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM conversations WHERE status = 'closed' {mailbox_clause}"),
        [],
        |r| r.get(0),
    )?;

    // Active conversations.
    let active: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM conversations WHERE status = 'active' {mailbox_clause}"),
        [],
        |r| r.get(0),
    )?;

    // Customer waiting (response_state = 'customer_waiting').
    let waiting: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations
             WHERE response_state = 'customer_waiting' {mailbox_clause}"
        ),
        [],
        |r| r.get(0),
    )?;

    // Avg first response time (minutes) — from the activity engine's derived column.
    let avg_first_response: Option<f64> = conn
        .query_row(
            &format!(
                "SELECT AVG((julianday(first_response_at) - julianday(created_at)) * 24 * 60)
                 FROM conversations
                 WHERE first_response_at IS NOT NULL AND created_at IS NOT NULL
                 {mailbox_clause}"
            ),
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    // Avg resolution time (minutes).
    let avg_resolution: Option<f64> = conn
        .query_row(
            &format!(
                "SELECT AVG((julianday(closed_at) - julianday(created_at)) * 24 * 60)
                 FROM conversations
                 WHERE closed_at IS NOT NULL AND created_at IS NOT NULL
                 AND status = 'closed' {mailbox_clause}"
            ),
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    // SLA breach count (from M7-T06).
    let sla_breaches: i64 = conn
        .query_row("SELECT COUNT(*) FROM sla_breaches", [], |r| r.get(0))
        .unwrap_or(0);

    Ok(DashboardMetrics {
        total_conversations: u32::try_from(total).unwrap_or(0),
        new_conversations: u32::try_from(new_conv).unwrap_or(0),
        closed_conversations: u32::try_from(closed).unwrap_or(0),
        active_conversations: u32::try_from(active).unwrap_or(0),
        customer_waiting: u32::try_from(waiting).unwrap_or(0),
        avg_first_response_minutes: avg_first_response,
        avg_resolution_minutes: avg_resolution,
        sla_breach_count: u32::try_from(sla_breaches).unwrap_or(0),
    })
}

// ─── M8-T02: Report builder ──────────────────────────────────────────────

/// A single row in a report result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportRow {
    pub dimension_value: String,
    pub metric_value: f64,
}

/// The result of a report query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportResult {
    pub metric: String,
    pub dimension: String,
    pub rows: Vec<ReportRow>,
    pub previous_period_rows: Vec<ReportRow>,
    pub metric_definition: String,
    pub metric_limitations: String,
}

/// Build a report. Per the reference notes: "Custom report builder (21 metrics ×
/// 14 dimensions) with previous-period comparison; metrics ship their own
/// definition + limitations in the response."
pub fn build_report(
    conn: &Connection,
    metric: ReportMetricKey,
    dimension: ReportDimensionKey,
    days_back: u32,
) -> Result<ReportResult> {
    let days_back = days_back.max(1) as i64;

    // Build the dimension GROUP BY clause based on the dimension key.
    let (dim_expr, dim_label) = match dimension {
        ReportDimensionKey::None => ("'all'".to_string(), "all".to_string()),
        ReportDimensionKey::Day => (
            "strftime('%Y-%m-%d', local_created_at)".to_string(),
            "day".to_string(),
        ),
        ReportDimensionKey::Week => (
            "strftime('%Y-W%W', local_created_at)".to_string(),
            "week".to_string(),
        ),
        ReportDimensionKey::Month => (
            "strftime('%Y-%m', local_created_at)".to_string(),
            "month".to_string(),
        ),
        ReportDimensionKey::Mailbox => ("mailbox_id".to_string(), "mailbox".to_string()),
        ReportDimensionKey::Status => ("status".to_string(), "status".to_string()),
        ReportDimensionKey::Assignee => ("assignee_id".to_string(), "assignee".to_string()),
        _ => ("'all'".to_string(), "all".to_string()), // fallback for dimensions not yet mapped
    };

    // Build the metric expression.
    let (metric_expr, definition, limitations) = metric_info(metric);

    // Current period query.
    let sql = format!(
        "SELECT {dim_expr} as dim, {metric_expr} as val
         FROM conversations
         WHERE julianday(local_created_at) >= julianday('now', '-{days_back} days')
         GROUP BY {dim_expr}
         ORDER BY {dim_expr}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let current_rows: Vec<ReportRow> = stmt
        .query_map([], |r| {
            Ok(ReportRow {
                dimension_value: r.get::<_, String>(0)?,
                metric_value: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    // Previous period query (same duration, shifted back).
    let prev_days_start = days_back * 2;
    let prev_sql = format!(
        "SELECT {dim_expr} as dim, {metric_expr} as val
         FROM conversations
         WHERE julianday(local_created_at) >= julianday('now', '-{prev_days_start} days')
           AND julianday(local_created_at) < julianday('now', '-{days_back} days')
         GROUP BY {dim_expr}
         ORDER BY {dim_expr}"
    );
    let mut prev_stmt = conn.prepare(&prev_sql)?;
    let prev_rows: Vec<ReportRow> = prev_stmt
        .query_map([], |r| {
            Ok(ReportRow {
                dimension_value: r.get::<_, String>(0)?,
                metric_value: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    Ok(ReportResult {
        metric: metric.as_str().to_string(),
        dimension: dim_label.to_string(),
        rows: current_rows,
        previous_period_rows: prev_rows,
        metric_definition: definition.to_string(),
        metric_limitations: limitations.to_string(),
    })
}

/// Get the metric SQL expression + definition + limitations for a metric key.
/// Per the reference changelog: "metrics ship their own definition + limitations
/// in the response."
#[must_use]
fn metric_info(metric: ReportMetricKey) -> (&'static str, &'static str, &'static str) {
    match metric {
        ReportMetricKey::Conversations => ("COUNT(*)", "Total number of conversations.", "Does not include deleted conversations."),
        ReportMetricKey::UniqueCustomers => ("COUNT(DISTINCT customer_id)", "Number of unique customers who contacted support.", "A customer with multiple conversations is counted once."),
        ReportMetricKey::Organizations => ("COUNT(DISTINCT customer_id)", "Approximate organization count (proxy via distinct customers).", "True organization count requires the customers table's organization field; this is a proxy."),
        ReportMetricKey::FirstResponses => ("COUNT(CASE WHEN first_response_at IS NOT NULL THEN 1 END)", "Number of conversations that received a first response.", "Only counts conversations with a recorded first_response_at."),
        ReportMetricKey::AgentReplies => ("COUNT(*)", "Total conversations (proxy for agent reply count).", "True agent reply count requires message-level data; this is a proxy at conversation level."),
        ReportMetricKey::CustomerReplies => ("COUNT(*)", "Total conversations (proxy for customer reply count).", "True customer reply count requires message-level data."),
        ReportMetricKey::Closures => ("COUNT(CASE WHEN status = 'closed' THEN 1 END)", "Number of closed conversations.", "Only counts conversations with status='closed'."),
        ReportMetricKey::AvgFirstResponseMinutes => ("AVG((julianday(first_response_at) - julianday(created_at)) * 24 * 60)", "Average time to first response in minutes.", "Only includes conversations with a first_response_at; NULL values are excluded."),
        ReportMetricKey::AvgResolutionMinutes => ("AVG((julianday(closed_at) - julianday(created_at)) * 24 * 60)", "Average time from creation to closure in minutes.", "Only includes closed conversations."),
        ReportMetricKey::AvgWaitHours => ("AVG((julianday('now') - julianday(customer_waiting_since)) * 24)", "Average customer waiting time in hours.", "Only includes conversations currently in 'customer_waiting' state."),
        ReportMetricKey::SlaBreached => ("COUNT(CASE WHEN status = 'closed' AND closed_at IS NOT NULL THEN 1 END)", "Proxy for SLA breaches (closed conversations).", "True SLA breach count comes from the sla_breaches table; this is a conversation-level proxy."),
        _ => ("COUNT(*)", "Generic count metric.", "This metric's specific definition is not yet implemented; falls back to conversation count."),
    }
}

// ─── M8-T03: Effectiveness ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectivenessReport {
    pub avg_qa_score: Option<f64>,
    pub total_qa_checks: u32,
    pub score_distribution: Vec<(String, u32)>,
}

/// Aggregate post-resolution QA results. Per the reference notes:
/// `quality.ts` handles "QA / friction / effectiveness."
pub fn get_effectiveness_report(conn: &Connection) -> Result<EffectivenessReport> {
    let avg_score: Option<f64> = conn
        .query_row(
            "SELECT AVG(qa_score) FROM post_resolution_qa WHERE qa_score IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    let total: i64 = conn.query_row("SELECT COUNT(*) FROM post_resolution_qa", [], |r| r.get(0))?;

    // Score distribution: bucket into 0-0.5, 0.5-0.75, 0.75-1.0.
    let mut stmt = conn.prepare(
        "SELECT
            CASE
                WHEN qa_score < 0.5 THEN 'low'
                WHEN qa_score < 0.75 THEN 'medium'
                ELSE 'high'
            END as bucket,
            COUNT(*) as cnt
         FROM post_resolution_qa
         WHERE qa_score IS NOT NULL
         GROUP BY bucket
         ORDER BY bucket",
    )?;
    let distribution: Vec<(String, u32)> = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32))
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    Ok(EffectivenessReport {
        avg_qa_score: avg_score,
        total_qa_checks: u32::try_from(total).unwrap_or(0),
        score_distribution: distribution,
    })
}

// ─── M8-T04: Friction ────────────────────────────────────────────────────

/// A friction score for a conversation. Per the reference notes:
/// `quality.ts` handles "friction."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrictionScore {
    pub conversation_id: i64,
    pub effort_score: f64,
    pub factors: FrictionFactors,
}

/// The factors contributing to the friction score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrictionFactors {
    pub message_count: u32,
    pub resolution_hours: Option<f64>,
    pub reopen_count: u32,
    pub escalated: bool,
}

/// The threshold above which a conversation is "high friction."
pub const HIGH_FRICTION_THRESHOLD: f64 = 0.6;

/// Compute the friction score for a conversation. Per the reference notes:
/// effort is derived from message count, time to resolution, reopen count,
/// and escalation. This is deterministic (zero AI).
pub fn compute_friction(conn: &Connection, conversation_id: i64) -> Result<FrictionScore> {
    // Message count (from activity_events).
    let message_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM activity_events WHERE conversation_id = ?1 AND event_type = 'message'",
            params![conversation_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Resolution time (hours) — from created_at to closed_at.
    let resolution_hours: Option<f64> = conn
        .query_row(
            "SELECT (julianday(closed_at) - julianday(created_at)) * 24
             FROM conversations WHERE remote_id = ?1 AND closed_at IS NOT NULL",
            params![conversation_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();

    // Reopen count (status changes from closed back to active).
    let reopen_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ticket_state_transitions
             WHERE conversation_remote_id = ?1 AND to_state = 'active'
             AND from_state = 'closed'",
            params![conversation_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Escalated (has an escalation_signal AI attribute or escalation_intent interaction signal).
    let escalated: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_attributes
             WHERE conversation_id = ?1 AND attribute_key = 'escalation_signal'
             AND value = 'yes')",
            params![conversation_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|v: i64| v != 0)
        .unwrap_or(false);

    let factors = FrictionFactors {
        message_count: u32::try_from(message_count).unwrap_or(0),
        resolution_hours,
        reopen_count: u32::try_from(reopen_count).unwrap_or(0),
        escalated,
    };

    // Compute effort score (0.0 to 1.0).
    // Each factor contributes a weighted portion:
    // - High message count (>20) → +0.3
    // - Long resolution (>48h) → +0.3
    // - Reopened → +0.2 per reopen (capped at 0.4)
    // - Escalated → +0.2
    let mut score = 0.0_f64;
    if factors.message_count > 20 {
        score += 0.3;
    }
    if let Some(hours) = resolution_hours {
        if hours > 48.0 {
            score += 0.3;
        }
    }
    score += (factors.reopen_count as f64 * 0.2).min(0.4);
    if factors.escalated {
        score += 0.2;
    }
    let effort_score = score.min(1.0);

    // Store the score.
    let factors_json = serde_json::to_string(&factors).unwrap_or_default();
    let _ = conn.execute(
        "INSERT OR REPLACE INTO friction_scores (conversation_id, effort_score, factors_json)
         VALUES (?1, ?2, ?3)",
        params![conversation_id, effort_score, factors_json],
    );

    Ok(FrictionScore {
        conversation_id,
        effort_score,
        factors,
    })
}

/// Count high-friction conversations. Used by the `HighFrictionRate` report metric.
pub fn count_high_friction(conn: &Connection) -> Result<u32> {
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM friction_scores WHERE effort_score >= {HIGH_FRICTION_THRESHOLD}"
        ),
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

// ─── M8-T05: Support health ──────────────────────────────────────────────

/// A 0–100 health score. Per the reference notes: "support health" is an
/// aggregate metric. The health score is advisory — per spec: "AI is always advisory."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthScore {
    pub score: f64,
    pub components: HealthComponents,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthComponents {
    pub sla_performance: f64,
    pub friction_level: f64,
    pub response_time_score: f64,
    pub resolution_rate: f64,
}

/// Compute the support health score. Combines SLA performance, friction,
/// response times, and resolution rates into a single 0–100 score.
pub fn compute_health(conn: &Connection, days_back: u32) -> Result<HealthScore> {
    let metrics = get_dashboard_metrics(conn, None, days_back)?;

    // SLA performance: fewer breaches = higher score.
    let sla_perf = if metrics.total_conversations > 0 {
        1.0 - (metrics.sla_breach_count as f64 / metrics.total_conversations as f64)
    } else {
        1.0
    };

    // Friction level: fewer high-friction conversations = higher score.
    let high_friction = count_high_friction(conn).unwrap_or(0);
    let friction = if metrics.total_conversations > 0 {
        1.0 - (high_friction as f64 / metrics.total_conversations as f64)
    } else {
        1.0
    };

    // Response time score: faster = higher. 30 min = perfect, 4h = 0.
    let response_time = metrics
        .avg_first_response_minutes
        .map(|mins| {
            if mins <= 30.0 {
                1.0
            } else if mins >= 240.0 {
                0.0
            } else {
                1.0 - (mins - 30.0) / 210.0
            }
        })
        .unwrap_or(1.0); // No data → neutral.

    // Resolution rate: closed / total.
    let resolution_rate = if metrics.total_conversations > 0 {
        metrics.closed_conversations as f64 / metrics.total_conversations as f64
    } else {
        0.0
    };

    let components = HealthComponents {
        sla_performance: sla_perf,
        friction_level: friction,
        response_time_score: response_time,
        resolution_rate,
    };

    // Weighted average → 0–100.
    let score =
        (sla_perf * 0.30 + friction * 0.25 + response_time * 0.25 + resolution_rate * 0.20) * 100.0;

    Ok(HealthScore { score, components })
}

// ─── M8-T06: Customer timeline ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub id: Option<i64>,
    pub customer_id: i64,
    pub event_type: String,
    pub event_data: Option<String>,
    pub occurred_at: String,
}

/// Record a timeline event. Per the reference notes: `timeline/` module handles
/// "Customer timeline."
pub fn record_timeline_event(
    conn: &Connection,
    customer_id: i64,
    event_type: &str,
    event_data: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO customer_timeline (customer_id, event_type, event_data_json)
         VALUES (?1, ?2, ?3)",
        params![customer_id, event_type, event_data],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get the timeline for a customer, ordered newest-first via `julianday()`.
pub fn get_timeline(conn: &Connection, customer_id: i64, limit: u32) -> Result<Vec<TimelineEvent>> {
    let limit = limit.clamp(1, 500);
    let mut stmt = conn.prepare(
        "SELECT id, customer_id, event_type, event_data_json, occurred_at
         FROM customer_timeline
         WHERE customer_id = ?1
         ORDER BY julianday(occurred_at) DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![customer_id, limit], |r| {
            Ok(TimelineEvent {
                id: r.get(0)?,
                customer_id: r.get(1)?,
                event_type: r.get(2)?,
                event_data: r.get(3)?,
                occurred_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── M8-T07: Support graph ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: Option<i64>,
    pub kind: String,
    pub entity_id: Option<i64>,
    pub label: Option<String>,
    pub properties: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: Option<i64>,
    pub source_id: i64,
    pub target_id: i64,
    pub edge_type: String,
    pub created_at: String,
}

/// Validate that a node kind is one of the 12 from the catalog.
pub fn validate_graph_node_kind(kind: &str) -> Result<GraphNodeKind> {
    for k in GraphNodeKind::ALL {
        if k.as_str() == kind {
            return Ok(k);
        }
    }
    Err(crate::error::Error::Config(format!(
        "unknown graph node kind: {kind}"
    )))
}

/// Add a graph node. Kind validated against `GraphNodeKind::ALL` (12 kinds from the catalog).
pub fn add_graph_node(
    conn: &Connection,
    kind: &str,
    entity_id: Option<i64>,
    label: Option<&str>,
    properties: Option<&str>,
) -> Result<i64> {
    validate_graph_node_kind(kind)?;
    conn.execute(
        "INSERT INTO graph_nodes (kind, entity_id, label, properties_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![kind, entity_id, label, properties],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Add a graph edge.
pub fn add_graph_edge(
    conn: &Connection,
    source_id: i64,
    target_id: i64,
    edge_type: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO graph_edges (source_id, target_id, edge_type)
         VALUES (?1, ?2, ?3)",
        params![source_id, target_id, edge_type],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Get neighbors of a node (both outgoing and incoming edges).
pub fn get_graph_neighbors(conn: &Connection, node_id: i64) -> Result<Vec<GraphNode>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT n.id, n.kind, n.entity_id, n.label, n.properties_json, n.created_at
         FROM graph_nodes n
         JOIN graph_edges e ON (e.target_id = n.id AND e.source_id = ?1)
                            OR (e.source_id = n.id AND e.target_id = ?1)
         WHERE n.id != ?1
         ORDER BY n.id",
    )?;
    let rows = stmt
        .query_map(params![node_id], |r| {
            Ok(GraphNode {
                id: r.get(0)?,
                kind: r.get(1)?,
                entity_id: r.get(2)?,
                label: r.get(3)?,
                properties: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Get a node by id.
pub fn get_graph_node(conn: &Connection, node_id: i64) -> Result<Option<GraphNode>> {
    // Use a struct to avoid the clippy type_complexity lint on a 6-tuple.
    struct NodeRow {
        id: i64,
        kind: String,
        entity_id: Option<i64>,
        label: Option<String>,
        properties: Option<String>,
        created_at: String,
    }
    let row: Option<NodeRow> = conn
        .query_row(
            "SELECT id, kind, entity_id, label, properties_json, created_at
             FROM graph_nodes WHERE id = ?1",
            params![node_id],
            |r| {
                Ok(NodeRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    entity_id: r.get(2)?,
                    label: r.get(3)?,
                    properties: r.get(4)?,
                    created_at: r.get(5)?,
                })
            },
        )
        .ok();
    match row {
        None => Ok(None),
        Some(r) => Ok(Some(GraphNode {
            id: Some(r.id),
            kind: r.kind,
            entity_id: r.entity_id,
            label: r.label,
            properties: r.properties,
            created_at: r.created_at,
        })),
    }
}

/// Count nodes by kind. Per the reference notes: 12 node kinds.
pub fn count_graph_nodes_by_kind(conn: &Connection, kind: &str) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM graph_nodes WHERE kind = ?1",
        params![kind],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::apply_m003;
    use crate::ai_features::apply_m011_to_m013;
    use crate::embeddings::apply_m008;
    use crate::intelligence::apply_m014;
    use crate::intelligence_features::apply_m015_to_m019;
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
        crate::jobs::ensure_jobs_table(&conn).unwrap();
        crate::notifications::apply_m005(&conn).unwrap();
        crate::side_threads::apply_m006(&conn).unwrap();
        crate::automation::apply_m007(&conn).unwrap();
        apply_m008(&conn).unwrap();
        crate::ai_center::apply_m009(&conn).unwrap();
        crate::ai_analysis::apply_m010(&conn).unwrap();
        apply_m011_to_m013(&conn).unwrap();
        apply_m014(&conn).unwrap();
        apply_m015_to_m019(&conn).unwrap();
        apply_m020_to_m022(&conn).unwrap();
        conn
    }

    fn insert_conversation(
        conn: &Connection,
        remote_id: i64,
        status: &str,
        mailbox_id: i64,
        customer_id: i64,
    ) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (?1, ?1, ?2, ?3, ?4)",
            params![remote_id, status, mailbox_id, customer_id],
        )
        .unwrap();
    }

    // ---- M020–M022 migrations ----------------------------------------------

    #[test]
    fn m020_creates_friction_scores_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM friction_scores", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m021_creates_customer_timeline_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM customer_timeline", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m022_creates_graph_tables() {
        let conn = fresh_db();
        let nodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))
            .unwrap();
        let edges: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(nodes, 0);
        assert_eq!(edges, 0);
    }

    #[test]
    fn m020_to_m022_is_idempotent() {
        let conn = fresh_db();
        apply_m020_to_m022(&conn).unwrap();
    }

    // ---- M8-T01: Dashboards -------------------------------------------------

    #[test]
    fn dashboard_metrics_empty_db() {
        let conn = fresh_db();
        let metrics = get_dashboard_metrics(&conn, None, 7).unwrap();
        assert_eq!(metrics.total_conversations, 0);
        assert_eq!(metrics.active_conversations, 0);
        assert_eq!(metrics.sla_breach_count, 0);
    }

    #[test]
    fn dashboard_metrics_with_conversations() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active", 101, 2001);
        insert_conversation(&conn, 1002, "closed", 101, 2002);
        insert_conversation(&conn, 1003, "active", 102, 2003);

        let metrics = get_dashboard_metrics(&conn, None, 7).unwrap();
        assert_eq!(metrics.total_conversations, 3);
        assert_eq!(metrics.closed_conversations, 1);
        assert_eq!(metrics.active_conversations, 2);
    }

    #[test]
    fn dashboard_metrics_filtered_by_mailbox() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active", 101, 2001);
        insert_conversation(&conn, 1002, "closed", 102, 2002);

        let metrics = get_dashboard_metrics(&conn, Some(101), 7).unwrap();
        assert_eq!(metrics.total_conversations, 1, "only mailbox 101");
    }

    // ---- M8-T02: Report builder --------------------------------------------

    #[test]
    fn build_report_conversations_by_status() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "active", 101, 2001);
        insert_conversation(&conn, 1002, "closed", 101, 2002);
        insert_conversation(&conn, 1003, "active", 101, 2003);

        let report = build_report(
            &conn,
            ReportMetricKey::Conversations,
            ReportDimensionKey::Status,
            365,
        )
        .unwrap();

        assert_eq!(report.metric, "conversations");
        assert_eq!(report.dimension, "status");
        assert!(!report.rows.is_empty(), "should have rows");
        assert!(!report.metric_definition.is_empty(), "definition included");
        assert!(
            !report.metric_limitations.is_empty(),
            "limitations included"
        );
    }

    #[test]
    fn build_report_empty_dataset() {
        let conn = fresh_db();
        let report = build_report(
            &conn,
            ReportMetricKey::Conversations,
            ReportDimensionKey::None,
            7,
        )
        .unwrap();
        assert!(report.rows.is_empty(), "empty DB → empty rows");
        assert!(report.previous_period_rows.is_empty());
    }

    // ---- M8-T03: Effectiveness ----------------------------------------------

    #[test]
    fn effectiveness_report_empty() {
        let conn = fresh_db();
        let report = get_effectiveness_report(&conn).unwrap();
        assert_eq!(report.total_qa_checks, 0);
        assert!(report.avg_qa_score.is_none());
    }

    #[test]
    fn effectiveness_report_with_data() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO post_resolution_qa (conversation_id, qa_score, qa_notes)
             VALUES (1001, 0.9, 'good'), (1002, 0.6, 'ok'), (1003, 0.3, 'poor')",
            [],
        )
        .unwrap();

        let report = get_effectiveness_report(&conn).unwrap();
        assert_eq!(report.total_qa_checks, 3);
        assert!(report.avg_qa_score.is_some());
        assert!((report.avg_qa_score.unwrap() - 0.6).abs() < 1e-6);
        assert!(!report.score_distribution.is_empty());
    }

    // ---- M8-T04: Friction --------------------------------------------------

    #[test]
    fn compute_friction_low_effort() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "closed", 101, 2001);
        // Set closed_at to make resolution fast (1 hour).
        conn.execute(
            "UPDATE conversations SET closed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
             created_at = strftime('%Y-%m-%dT%H:%M:%fZ','now','-2 hours')
             WHERE remote_id = 1001",
            [],
        )
        .unwrap();

        let score = compute_friction(&conn, 1001).unwrap();
        assert!(
            score.effort_score < HIGH_FRICTION_THRESHOLD,
            "low effort → low score"
        );
        assert_eq!(score.factors.message_count, 0);
    }

    #[test]
    fn high_friction_threshold_is_0_6() {
        assert_eq!(HIGH_FRICTION_THRESHOLD, 0.6);
    }

    #[test]
    fn count_high_friction_empty() {
        let conn = fresh_db();
        assert_eq!(count_high_friction(&conn).unwrap(), 0);
    }

    // ---- M8-T05: Support health --------------------------------------------

    #[test]
    fn compute_health_empty_db() {
        let conn = fresh_db();
        let health = compute_health(&conn, 7).unwrap();
        // Empty DB → all components neutral (1.0 except resolution_rate=0).
        assert!(health.score > 0.0, "health score > 0 even on empty DB");
        assert!(health.score <= 100.0);
    }

    #[test]
    fn compute_health_score_in_range() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "closed", 101, 2001);
        let health = compute_health(&conn, 7).unwrap();
        assert!(health.score >= 0.0 && health.score <= 100.0);
    }

    // ---- M8-T06: Customer timeline -----------------------------------------

    #[test]
    fn record_and_get_timeline_event() {
        let conn = fresh_db();
        let id = record_timeline_event(
            &conn,
            2001,
            "conversation_created",
            Some(r#"{"conv_id": 1001}"#),
        )
        .unwrap();
        assert!(id > 0);

        let events = get_timeline(&conn, 2001, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "conversation_created");
    }

    #[test]
    fn get_timeline_empty_for_nonexistent_customer() {
        let conn = fresh_db();
        assert!(get_timeline(&conn, 9999, 10).unwrap().is_empty());
    }

    #[test]
    fn timeline_ordered_newest_first() {
        let conn = fresh_db();
        record_timeline_event(&conn, 2001, "first", None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        record_timeline_event(&conn, 2001, "second", None).unwrap();

        let events = get_timeline(&conn, 2001, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "second", "newest first");
    }

    // ---- M8-T07: Support graph ----------------------------------------------

    #[test]
    fn add_graph_node_validates_kind_works() {
        let conn = fresh_db();
        // Valid kind.
        let id = add_graph_node(&conn, "customer", Some(2001), Some("Alice"), None).unwrap();
        assert!(id > 0);
        // Invalid kind.
        assert!(add_graph_node(&conn, "invalid_kind", None, None, None).is_err());
    }

    #[test]
    fn add_graph_node_and_get_works() {
        let conn = fresh_db();
        let id =
            add_graph_node(&conn, "conversation", Some(1001), Some("Bug report"), None).unwrap();
        let node = get_graph_node(&conn, id).unwrap().unwrap();
        assert_eq!(node.kind, "conversation");
        assert_eq!(node.label.as_deref(), Some("Bug report"));
    }

    #[test]
    fn get_graph_node_returns_none_for_nonexistent() {
        let conn = fresh_db();
        assert!(get_graph_node(&conn, 9999).unwrap().is_none());
    }

    #[test]
    fn add_graph_edge_and_get_neighbors_works() {
        let conn = fresh_db();
        let n1 = add_graph_node(&conn, "customer", Some(2001), Some("Alice"), None).unwrap();
        let n2 = add_graph_node(&conn, "conversation", Some(1001), Some("Bug"), None).unwrap();
        let n3 = add_graph_node(&conn, "known_issue", Some(1), Some("Login bug"), None).unwrap();

        // Alice → Bug → Login bug.
        add_graph_edge(&conn, n1, n2, "filed").unwrap();
        add_graph_edge(&conn, n2, n3, "linked_to").unwrap();

        // n2's neighbors should include n1 and n3.
        let neighbors = get_graph_neighbors(&conn, n2).unwrap();
        assert_eq!(neighbors.len(), 2, "both incoming and outgoing neighbors");
    }

    #[test]
    fn count_graph_nodes_by_kind_works() {
        let conn = fresh_db();
        add_graph_node(&conn, "customer", Some(1), None, None).unwrap();
        add_graph_node(&conn, "customer", Some(2), None, None).unwrap();
        add_graph_node(&conn, "conversation", Some(1001), None, None).unwrap();

        assert_eq!(count_graph_nodes_by_kind(&conn, "customer").unwrap(), 2);
        assert_eq!(count_graph_nodes_by_kind(&conn, "conversation").unwrap(), 1);
        assert_eq!(count_graph_nodes_by_kind(&conn, "incident").unwrap(), 0);
    }

    #[test]
    fn validate_all_12_graph_node_kinds() {
        // All 12 kinds from the catalog should be valid.
        for kind in GraphNodeKind::ALL {
            assert!(
                validate_graph_node_kind(kind.as_str()).is_ok(),
                "{kind:?} should be valid"
            );
        }
        assert!(validate_graph_node_kind("invalid").is_err());
    }

    // ---- serde --------------------------------------------------------------

    #[test]
    fn dashboard_metrics_serializes() {
        let m = DashboardMetrics::default();
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"total_conversations\":0"));
    }

    #[test]
    fn health_score_serializes() {
        let h = HealthScore {
            score: 75.0,
            components: HealthComponents {
                sla_performance: 0.9,
                friction_level: 0.8,
                response_time_score: 0.7,
                resolution_rate: 0.6,
            },
        };
        let s = serde_json::to_string(&h).unwrap();
        assert!(s.contains("\"score\":75.0"));
    }

    #[test]
    fn graph_node_serializes() {
        let n = GraphNode {
            id: Some(1),
            kind: "customer".into(),
            entity_id: Some(2001),
            label: Some("Alice".into()),
            properties: None,
            created_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&n).unwrap();
        assert!(s.contains("\"kind\":\"customer\""));
    }
}
