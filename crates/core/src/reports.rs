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

    -- M022: graph_nodes (the human-edge store `graph_edges` — the
    -- reference's support_graph_edges under the documented rename — is
    -- created by support_graph::ensure_graph_edges_schema in the reference
    -- migration-016 shape; GR-02)
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

    UPDATE app_state SET schema_version = 22 WHERE id = 1;
"#;

/// Apply M020–M022 migrations. Idempotent. The human-edge store
/// (graph_edges, the reference's support_graph_edges) is ensured in the
/// reference migration-016 shape — including the reshape of pre-GR-02
/// legacy databases whose graph_edges was a graph_nodes-surrogate model
/// written only by the old unvalidated route.
pub fn apply_m020_to_m022(conn: &Connection) -> Result<()> {
    conn.execute_batch(M020_TO_M022_SQL)?;
    crate::support_graph::ensure_graph_edges_schema(conn)?;
    Ok(())
}

/// M034: saved custom report definitions (reference migration 015, Phase 33
/// block — exact reference DDL including the `provenance` column).
pub const M034_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS report_definitions (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        name       TEXT NOT NULL,
        config     TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
        provenance TEXT NOT NULL DEFAULT 'human_local'
    );
    UPDATE app_state SET schema_version = 34 WHERE id = 1;
"#;

/// Apply the M034 migration. Idempotent.
pub fn apply_m034(conn: &Connection) -> Result<()> {
    conn.execute_batch(M034_SQL)?;
    ensure_mailbox_business_hours(conn)?;
    Ok(())
}

/// Ensure the `mailbox_business_hours` table exists (the `sla_breached`
/// report metric reads its `first_response_target_min`). Same DDL as the
/// settings route's ensure helper — both are idempotent `CREATE IF NOT
/// EXISTS`, so order does not matter.
pub fn ensure_mailbox_business_hours(conn: &Connection) -> Result<()> {
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
        );",
    )?;
    Ok(())
}

/// Ensure the `report_definitions` table exists (idempotent; safe to call from
/// route handlers so saved-report reads never 500 on a DB booted before
/// M034 landed).
pub fn ensure_report_definitions_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS report_definitions (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            name       TEXT NOT NULL,
            config     TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            provenance TEXT NOT NULL DEFAULT 'human_local'
        );",
    )?;
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

// ─── Custom report builder (v2.1.0, plan Phase 33) ───────────────────────
//
// Port of the reference `ReportBuilderService` (src/server/analytics/
// reportBuilder.ts). Compilation policy (the same one as the view engine and
// tile fragments): metric keys, dimension keys and every SQL identifier come
// from CLOSED code-side catalogs; only VALUES are bound parameters. A user
// config selects catalog entries — it can never contribute SQL text.

/// Conversation-level filters applied identically by the aggregate query and
/// the sample-conversation query (reference `ReportFilters`). `None` = absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportFilters {
    pub mailbox_local_ids: Option<Vec<i64>>,
    pub channel: Option<String>,
    pub tags_any: Option<Vec<String>>,
    pub tags_none: Option<Vec<String>>,
    pub statuses: Option<Vec<String>>,
    pub assignee_local_ids: Option<Vec<i64>>,
    pub min_priority: Option<String>,
    /// For ai_attribute_share: which catalog attribute the metric measures.
    pub attribute_key: Option<String>,
    /// For ai_attribute_share / avg_state_hours: closed value / state key.
    pub attribute_value: Option<String>,
    pub state_key: Option<String>,
}

/// The comparison mode for a report run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReportComparison {
    #[default]
    None,
    PreviousPeriod,
}

/// The row ordering for a report run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReportSort {
    #[default]
    MetricDesc,
    MetricAsc,
    DimensionAsc,
}

/// A validated report configuration (reference `ReportConfig`).
#[derive(Debug, Clone, PartialEq)]
pub struct ReportBuilderConfig {
    pub metric: ReportMetricKey,
    pub dimension: ReportDimensionKey,
    pub date_from: String,
    pub date_to: String,
    pub comparison: ReportComparison,
    pub filters: ReportFilters,
    pub sort: ReportSort,
    pub limit: Option<i64>,
}

/// One row of a report result (reference `ReportRow`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportBuilderRow {
    pub dimension_value: String,
    pub dimension_label: String,
    pub value: f64,
    pub sample_conversation_ids: Vec<i64>,
}

/// An inclusive date range (reference `{ dateFrom, dateTo }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportDateRange {
    #[serde(rename = "dateFrom")]
    pub date_from: String,
    #[serde(rename = "dateTo")]
    pub date_to: String,
}

/// The result of a report run (reference `ReportRunResult`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportRunOutcome {
    pub metric: crate::catalog::reporting::MetricCatalogEntry,
    pub dimension: crate::catalog::reporting::DimensionCatalogEntry,
    pub rows: Vec<ReportBuilderRow>,
    pub comparison_rows: Option<Vec<ReportBuilderRow>>,
    pub comparison_range: Option<ReportDateRange>,
    pub date_range: ReportDateRange,
    pub notes: Vec<String>,
    pub origin: &'static str,
}

/// A saved report definition (reference `SavedReport`; `config` is the parsed
/// JSON config).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedReport {
    pub id: i64,
    pub name: String,
    pub config: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Which table family a metric aggregates over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricAnchor {
    Conversations,
    Threads,
    Transitions,
    Outcomes,
    Recipients,
}

/// The compiled form of one metric: FROM clause, anchor timestamp, SELECT
/// value expression and extra WHERE clauses — all closed-catalog constants
/// mapped onto the port's schema.
struct MetricSpec {
    anchor: MetricAnchor,
    from: &'static str,
    date_expr: &'static str,
    value_expr: &'static str,
    extra_where: &'static [&'static str],
    requires_attribute: bool,
    requires_state: bool,
    total_only: bool,
}

impl MetricSpec {
    /// Whether the FROM clause exposes the `conversations c` alias (dimension
    /// joins reference `c`). Mirrors the reference's
    /// `spec.from.includes('conversations c')` check.
    fn has_conversation_alias(&self) -> bool {
        self.from.contains("conversations c")
    }

    /// Whether conversation-level filters apply (reference:
    /// conversations/threads anchors only — NOT outcomes).
    fn applies_conversation_filters(&self) -> bool {
        matches!(
            self.anchor,
            MetricAnchor::Conversations | MetricAnchor::Threads
        )
    }

    /// Whether sample conversations may be attached (reference:
    /// conversation-anchored FROMs only).
    fn is_conversation_anchored(&self) -> bool {
        matches!(
            self.anchor,
            MetricAnchor::Conversations | MetricAnchor::Threads | MetricAnchor::Outcomes
        )
    }
}

/// The compiled form of one dimension: SELECT expression (alias `dv`),
/// GROUP BY expression, and extra joins. `{DATE}` is replaced by the metric's
/// anchor timestamp expression at compile time.
struct DimensionSpec {
    expr: &'static str,
    group_by: &'static str,
    joins: &'static [&'static str],
}

/// The priority rank used by the `minPriority` filter (none < low < normal <
/// high < urgent), mirroring the reference's PRIORITY_RANK.
fn priority_rank(priority: &str) -> i64 {
    match priority {
        "low" => 1,
        "normal" => 2,
        "high" => 3,
        "urgent" => 4,
        _ => 0,
    }
}

/// The value-expression SQL for the `ai_attribute_share` metric when a
/// concrete value is configured (caller binds attributeKey + attributeValue
/// after the range params — the SELECT expression precedes the WHERE clause
/// in the final SQL, so its placeholders bind first).
fn attribute_share_expr() -> &'static str {
    "CAST(SUM(CASE WHEN EXISTS (\
      SELECT 1 FROM ai_attributes a WHERE a.conversation_id = c.id AND a.attribute = ? AND a.superseded_at IS NULL AND LOWER(a.value) = LOWER(?)\
    ) THEN 1 ELSE 0 END) AS REAL) / COUNT(*)"
}

/// The value-expression SQL for `ai_attribute_share` when the configured
/// value means "is unknown" (no current attribute row). Caller binds just
/// the attributeKey.
fn attribute_unknown_expr() -> &'static str {
    "CAST(SUM(CASE WHEN NOT EXISTS (\
      SELECT 1 FROM ai_attributes a WHERE a.conversation_id = c.id AND a.attribute = ? AND a.superseded_at IS NULL\
    ) THEN 1 ELSE 0 END) AS REAL) / COUNT(*)"
}

/// The metric spec table, mapped onto the port's schema:
/// - reference `remote_created_at` → port `conversations.created_at`
/// - `customer_local_id`/`mailbox_local_id`/`assignee_local_id` → `customer_id`/
///   `mailbox_id`/`assignee_id`
/// - `threads` → `conversation_threads` (`type` → `thread_type`;
///   `deleted_at` + `state` filtered like the reference since the mirror
///   carries both columns)
/// - `client_support_outcomes` → `friction_scores` (effort_score; high
///   friction = effort_score >= 0.6 per HIGH_FRICTION_THRESHOLD)
/// - `known_issue_conversations`/`issue_cluster_conversations` →
///   `known_issue_links`/`issue_cluster_members`
fn metric_spec(metric: ReportMetricKey) -> MetricSpec {
    const NO_WHERE: &[&str] = &[];
    match metric {
        ReportMetricKey::Conversations => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            value_expr: "COUNT(*)",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::UniqueCustomers => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            value_expr: "COUNT(DISTINCT c.customer_id)",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::Organizations => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            // Organizations live on customers, not conversations: resolve
            // through the conversation's customer so DISTINCT counts distinct
            // customer orgs (the port stores the org NAME on customers).
            value_expr: "COUNT(DISTINCT (SELECT cu.organization FROM customers cu WHERE cu.id = c.customer_id))",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::FirstResponses => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.first_response_at",
            value_expr: "COUNT(*)",
            extra_where: &["c.first_response_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AgentReplies => MetricSpec {
            anchor: MetricAnchor::Threads,
            from: "conversation_threads t JOIN conversations c ON c.id = t.conversation_id",
            date_expr: "t.created_at",
            value_expr: "COUNT(*)",
            extra_where: &[
                "t.thread_type = 'reply'",
                "t.deleted_at IS NULL",
                "t.state = 'published'",
            ],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::CustomerReplies => MetricSpec {
            anchor: MetricAnchor::Threads,
            from: "conversation_threads t JOIN conversations c ON c.id = t.conversation_id",
            date_expr: "t.created_at",
            value_expr: "COUNT(*)",
            extra_where: &[
                "t.thread_type = 'customer_message'",
                "t.deleted_at IS NULL",
                "t.state = 'published'",
            ],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::Closures => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.closed_at",
            value_expr: "COUNT(*)",
            extra_where: &["c.closed_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AvgFirstResponseMinutes => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.first_response_at",
            value_expr: "AVG((julianday(c.first_response_at) - julianday(c.created_at)) * 1440)",
            extra_where: &["c.first_response_at IS NOT NULL", "c.created_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AvgResolutionMinutes => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.closed_at",
            value_expr: "AVG((julianday(c.closed_at) - julianday(c.created_at)) * 1440)",
            extra_where: &["c.closed_at IS NOT NULL", "c.created_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AvgWaitHours => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.closed_at",
            value_expr: "AVG((julianday(c.closed_at) - julianday(c.last_customer_reply_at)) * 24)",
            extra_where: &[
                "c.closed_at IS NOT NULL",
                "c.last_customer_reply_at IS NOT NULL",
                "c.status = 'closed'",
            ],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::SlaBreached => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            value_expr: "COUNT(*)",
            extra_where: &[
                "c.first_response_at IS NOT NULL AND c.created_at IS NOT NULL",
                "(julianday(c.first_response_at) - julianday(c.created_at)) * 1440 > (SELECT mbh.first_response_target_min FROM mailbox_business_hours mbh WHERE mbh.mailbox_local_id = c.mailbox_id AND mbh.first_response_target_min IS NOT NULL)",
            ],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::StateChanges => MetricSpec {
            anchor: MetricAnchor::Transitions,
            from: "ticket_state_transitions tr",
            date_expr: "tr.transitioned_at",
            value_expr: "COUNT(*)",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AvgStateHours => MetricSpec {
            anchor: MetricAnchor::Transitions,
            // The port's transitions key on conversation_remote_id (the Help
            // Scout remote id), not the local conversations.id.
            from: "ticket_state_transitions tr JOIN conversations c ON c.remote_id = tr.conversation_remote_id",
            date_expr: "tr.transitioned_at",
            value_expr: "AVG((julianday((SELECT MIN(nx.transitioned_at) FROM ticket_state_transitions nx WHERE nx.conversation_remote_id = tr.conversation_remote_id AND nx.transitioned_at > tr.transitioned_at)) - julianday(tr.transitioned_at)) * 24)",
            extra_where: &["tr.to_state = ?"],
            requires_attribute: false,
            requires_state: true,
            total_only: false,
        },
        ReportMetricKey::HighPriorityRate => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            value_expr: "CAST(SUM(CASE WHEN c.supportos_priority IN ('high','urgent') THEN 1 ELSE 0 END) AS REAL) / COUNT(*)",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::IssueLinkedShare => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            value_expr: "CAST(SUM(CASE WHEN (EXISTS (SELECT 1 FROM issue_cluster_members icc WHERE icc.conversation_id = c.id) OR EXISTS (SELECT 1 FROM known_issue_links kic WHERE kic.conversation_id = c.id)) THEN 1 ELSE 0 END) AS REAL) / COUNT(*)",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AiAttributeShare => MetricSpec {
            anchor: MetricAnchor::Conversations,
            from: "conversations c",
            date_expr: "c.created_at",
            value_expr: "",
            extra_where: NO_WHERE,
            requires_attribute: true,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::AvgCustomerEffort => MetricSpec {
            anchor: MetricAnchor::Outcomes,
            from: "friction_scores o JOIN conversations c ON c.id = o.conversation_id",
            date_expr: "c.created_at",
            value_expr: "AVG(o.effort_score)",
            extra_where: &["o.effort_score IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::HighFrictionRate => MetricSpec {
            anchor: MetricAnchor::Outcomes,
            from: "friction_scores o JOIN conversations c ON c.id = o.conversation_id",
            date_expr: "c.created_at",
            value_expr: "CAST(SUM(CASE WHEN o.effort_score >= 0.6 THEN 1 ELSE 0 END) AS REAL) / COUNT(*)",
            extra_where: NO_WHERE,
            requires_attribute: false,
            requires_state: false,
            total_only: false,
        },
        ReportMetricKey::CampaignSent => MetricSpec {
            anchor: MetricAnchor::Recipients,
            from: "outreach_recipients r",
            date_expr: "r.sent_at",
            value_expr: "COUNT(*)",
            extra_where: &["r.sent_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: true,
        },
        ReportMetricKey::CampaignReplies => MetricSpec {
            anchor: MetricAnchor::Recipients,
            from: "outreach_recipients r",
            date_expr: "r.replied_at",
            value_expr: "COUNT(*)",
            extra_where: &["r.replied_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: true,
        },
        ReportMetricKey::CampaignReplyRate => MetricSpec {
            anchor: MetricAnchor::Recipients,
            from: "outreach_recipients r",
            date_expr: "r.sent_at",
            value_expr: "CAST(SUM(CASE WHEN r.replied_at IS NOT NULL THEN 1 ELSE 0 END) AS REAL) / COUNT(*)",
            extra_where: &["r.sent_at IS NOT NULL"],
            requires_attribute: false,
            requires_state: false,
            total_only: true,
        },
    }
}

/// The dimension spec table, mapped onto the port's schema. The port's
/// `teams` membership comes from the Help Scout provider at sync
/// (`team_members`), and `channel` resolves via `source_type` — both
/// expressions mirror the reference reportBuilder dimension SQL.
fn dimension_spec(dimension: ReportDimensionKey) -> DimensionSpec {
    const NO_JOINS: &[&str] = &[];
    match dimension {
        ReportDimensionKey::None => DimensionSpec {
            expr: "'(total)'",
            group_by: "'(total)'",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Day => DimensionSpec {
            expr: "strftime('%Y-%m-%d', {DATE})",
            group_by: "strftime('%Y-%m-%d', {DATE})",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Week => DimensionSpec {
            expr: "strftime('%Y-%W', {DATE})",
            group_by: "strftime('%Y-%W', {DATE})",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Month => DimensionSpec {
            expr: "strftime('%Y-%m', {DATE})",
            group_by: "strftime('%Y-%m', {DATE})",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Mailbox => DimensionSpec {
            expr: "COALESCE((SELECT m.name FROM mailboxes m WHERE m.id = c.mailbox_id), '(no mailbox)')",
            group_by: "c.mailbox_id",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Channel => DimensionSpec {
            // Channel via `source_type` — the same expression the reference
            // groups by (reportBuilder.ts:177); a NULL/empty source_type
            // carries the '(unknown channel)' label.
            expr: "COALESCE(NULLIF(c.source_type, ''), '(unknown channel)')",
            group_by: "COALESCE(NULLIF(c.source_type, ''), '(unknown channel)')",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Tag => DimensionSpec {
            expr: "tg.name",
            group_by: "tg.name",
            joins: &[
                "JOIN conversation_tags ct ON ct.conversation_id = c.id",
                "JOIN tags tg ON tg.id = ct.tag_id",
            ],
        },
        ReportDimensionKey::Assignee => DimensionSpec {
            expr: "COALESCE((SELECT (u.first_name || ' ' || u.last_name) FROM users u WHERE u.id = c.assignee_id), '(unassigned)')",
            group_by: "c.assignee_id",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Team => DimensionSpec {
            // MAIN groups on `c.assigned_team_local_id`; the port's
            // conversations mirror carries no assigned-team column, so the
            // team resolves through `team_members` of the assignee (the
            // membership the sync populates) — the same resolution MAIN's
            // report builder uses for its `team` dimension
            // (reportBuilder.ts:184-187).
            expr: "COALESCE((SELECT tm2.name FROM team_members tm JOIN teams tm2 ON tm2.id = tm.team_id WHERE tm.user_id = c.assignee_id LIMIT 1), '(no team)')",
            group_by: "COALESCE((SELECT tm.team_id FROM team_members tm WHERE tm.user_id = c.assignee_id LIMIT 1), -1)",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Status => DimensionSpec {
            expr: "c.status",
            group_by: "c.status",
            joins: NO_JOINS,
        },
        ReportDimensionKey::Priority => DimensionSpec {
            expr: "COALESCE(NULLIF(c.supportos_priority, ''), 'none')",
            group_by: "COALESCE(NULLIF(c.supportos_priority, ''), 'none')",
            joins: NO_JOINS,
        },
        ReportDimensionKey::CustomState => DimensionSpec {
            expr: "COALESCE((SELECT ts.name FROM ticket_states ts WHERE ts.id = c.supportos_state_id), '(no state)')",
            group_by: "c.supportos_state_id",
            joins: NO_JOINS,
        },
        ReportDimensionKey::ResponseState => DimensionSpec {
            expr: crate::response_state_sql::RESPONSE_STATE_SQL,
            group_by: crate::response_state_sql::RESPONSE_STATE_SQL,
            joins: NO_JOINS,
        },
        ReportDimensionKey::Issue => DimensionSpec {
            expr: "COALESCE(\
                (SELECT ('KI: ' || ki.name) FROM known_issue_links kil JOIN known_issues ki ON ki.id = kil.known_issue_id WHERE kil.conversation_id = c.id LIMIT 1), \
                (SELECT ('Cluster: ' || ic2.name) FROM issue_cluster_members icm JOIN issue_clusters ic2 ON ic2.id = icm.cluster_id WHERE icm.conversation_id = c.id LIMIT 1), \
                '(not linked to an issue)')",
            group_by: "COALESCE(\
                (SELECT kil.known_issue_id FROM known_issue_links kil WHERE kil.conversation_id = c.id LIMIT 1), \
                (SELECT icm.cluster_id FROM issue_cluster_members icm WHERE icm.conversation_id = c.id LIMIT 1), \
                -1)",
            joins: NO_JOINS,
        },
    }
}

/// Defense in depth (reference `safeSql`): the compiled SQL is built from
/// closed catalogs only; this guard refuses anything that still smells like
/// an injection attempt in a VALUE-less position.
fn safe_sql(sql: &str) -> std::result::Result<String, String> {
    if sql.contains(';')
        || sql.contains("--")
        || sql.contains("/*")
        || sql.to_ascii_lowercase().contains(" drop ")
        || sql.to_ascii_lowercase().contains(" delete ")
        || sql.to_ascii_lowercase().contains(" insert ")
        || sql.to_ascii_lowercase().contains(" update ")
    {
        return Err("Compiled report SQL failed the safety guard.".to_string());
    }
    Ok(sql.to_string())
}

/// A bound parameter for the compiled report SQL.
type SqlParam = rusqlite::types::Value;

/// Shared conversation-level filters (reference
/// `applyConversationFilters`): applied identically by the aggregate query
/// and the sample-conversation query so samples always respect the same
/// filters as the numbers they illustrate. Arrays are filtered FIRST so
/// placeholder count always equals the number of bound params.
fn apply_conversation_filters(
    f: &ReportFilters,
    where_sql: &mut Vec<String>,
    params: &mut Vec<SqlParam>,
) {
    if let Some(ids) = &f.mailbox_local_ids {
        let ids: Vec<i64> = ids.iter().copied().filter(|n| *n > 0).collect();
        if !ids.is_empty() {
            let marks: Vec<&str> = ids.iter().map(|_| "?").collect();
            where_sql.push(format!("c.mailbox_id IN ({})", marks.join(",")));
            params.extend(ids.iter().copied().map(SqlParam::Integer));
        }
    }
    if let Some(channel) = &f.channel {
        let trimmed = channel.trim();
        if !trimmed.is_empty() {
            // Channel via source_type (reference applyConversationFilters,
            // reportBuilder.ts:381-383).
            where_sql.push("LOWER(COALESCE(c.source_type, '')) = LOWER(?)".to_string());
            params.push(SqlParam::Text(trimmed.to_string()));
        }
    }
    if let Some(tags) = &f.tags_any {
        let tags: Vec<&String> = tags.iter().filter(|t| !t.is_empty()).collect();
        if !tags.is_empty() {
            let marks: Vec<&str> = tags.iter().map(|_| "?").collect();
            where_sql.push(format!(
                "EXISTS (SELECT 1 FROM conversation_tags ct2 JOIN tags tg2 ON tg2.id = ct2.tag_id WHERE ct2.conversation_id = c.id AND LOWER(tg2.name) IN ({}))",
                marks.join(",")
            ));
            params.extend(tags.iter().map(|t| SqlParam::Text(t.to_lowercase())));
        }
    }
    if let Some(tags) = &f.tags_none {
        let tags: Vec<&String> = tags.iter().filter(|t| !t.is_empty()).collect();
        if !tags.is_empty() {
            let marks: Vec<&str> = tags.iter().map(|_| "?").collect();
            where_sql.push(format!(
                "NOT EXISTS (SELECT 1 FROM conversation_tags ct2 JOIN tags tg2 ON tg2.id = ct2.tag_id WHERE ct2.conversation_id = c.id AND LOWER(tg2.name) IN ({}))",
                marks.join(",")
            ));
            params.extend(tags.iter().map(|t| SqlParam::Text(t.to_lowercase())));
        }
    }
    if let Some(statuses) = &f.statuses {
        let statuses: Vec<&String> = statuses.iter().filter(|s| !s.is_empty()).collect();
        if !statuses.is_empty() {
            let marks: Vec<&str> = statuses.iter().map(|_| "?").collect();
            where_sql.push(format!("c.status IN ({})", marks.join(",")));
            params.extend(statuses.iter().map(|s| SqlParam::Text((*s).clone())));
        }
    }
    if let Some(ids) = &f.assignee_local_ids {
        let ids: Vec<i64> = ids.to_vec();
        if !ids.is_empty() {
            let marks: Vec<&str> = ids.iter().map(|_| "?").collect();
            where_sql.push(format!("c.assignee_id IN ({})", marks.join(",")));
            params.extend(ids.iter().copied().map(SqlParam::Integer));
        }
    }
    if let Some(min_priority) = &f.min_priority {
        let rank = priority_rank(min_priority);
        if rank > 0 {
            where_sql.push(
                "CASE c.supportos_priority WHEN 'low' THEN 1 WHEN 'normal' THEN 2 WHEN 'high' THEN 3 WHEN 'urgent' THEN 4 ELSE 0 END >= ?"
                    .to_string(),
            );
            params.push(SqlParam::Integer(rank));
        }
    }
}

/// Sample conversations for one report row (bounded to 3, newest first).
/// Samples bind the RAW group key (gk) — the groupBy expression's own
/// value — never the display label. Any SQL failure yields an empty list
/// (samples are illustrative, never load-bearing).
fn sample_conversations(
    conn: &Connection,
    config: &ReportBuilderConfig,
    spec: &MetricSpec,
    dim: &DimensionSpec,
    group_key: Option<&rusqlite::types::Value>,
) -> Vec<i64> {
    let mut params: Vec<SqlParam> = vec![
        SqlParam::Text(config.date_from.clone()),
        SqlParam::Text(config.date_to.clone()),
    ];
    let mut where_sql: Vec<String> = vec![
        format!(
            "COALESCE(julianday({}), julianday('2000-01-01')) >= julianday(?)",
            spec.date_expr
        ),
        format!(
            "COALESCE(julianday({}), julianday('2000-01-01')) < julianday(?, '+1 day')",
            spec.date_expr
        ),
    ];
    for w in spec.extra_where {
        if *w == "tr.to_state = ?" {
            params.push(SqlParam::Text(
                config.filters.state_key.clone().unwrap_or_default(),
            ));
        }
        where_sql.push((*w).to_string());
    }
    apply_conversation_filters(&config.filters, &mut where_sql, &mut params);
    if let Some(gk) = group_key {
        let gk_expr = dim.group_by.replace("{DATE}", spec.date_expr);
        if matches!(gk, rusqlite::types::Value::Null) {
            // A NULL group key ('(unassigned)', '(no mailbox)'...) matches IS
            // NULL, never '= value'.
            where_sql.push(format!("{gk_expr} IS NULL"));
        } else {
            where_sql.push(format!("{gk_expr} = ?"));
            params.push(gk.clone());
        }
    }
    let joins = dim.joins.join(" ");
    let sql = format!(
        "SELECT DISTINCT c.id FROM {} {} WHERE {} ORDER BY c.id DESC LIMIT 3",
        spec.from,
        joins,
        where_sql.join(" AND ")
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return vec![];
    };
    let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        r.get::<_, i64>(0)
    }) else {
        return vec![];
    };
    rows.filter_map(std::result::Result::ok).collect()
}

/// Execute the compiled aggregate query for one range (reference
/// `executeQuery`). Returns the rows in GROUP BY output order (the caller
/// sorts + limits afterwards, exactly like the reference).
fn execute_builder_query(
    conn: &Connection,
    config: &ReportBuilderConfig,
    spec: &MetricSpec,
    date_from: &str,
    date_to: &str,
) -> std::result::Result<Vec<ReportBuilderRow>, String> {
    let dim = dimension_spec(config.dimension);
    let date_expr = spec.date_expr;
    let dim_expr = dim.expr.replace("{DATE}", date_expr);
    let group_expr = dim.group_by.replace("{DATE}", date_expr);
    let joins: Vec<&str> = if spec.has_conversation_alias() {
        dim.joins.to_vec()
    } else {
        Vec::new()
    };

    // Parameters bind by POSITION in the final SQL: the SELECT value
    // expression comes before the WHERE clause, so value-expression params
    // must precede range/filter params (SQLite binds ? in string order).
    let mut select_params: Vec<SqlParam> = Vec::new();
    let mut where_params: Vec<SqlParam> = vec![
        SqlParam::Text(date_from.to_string()),
        SqlParam::Text(date_to.to_string()),
    ];
    let mut where_sql: Vec<String> = vec![
        format!(
            "COALESCE(julianday({}), julianday('2000-01-01')) >= julianday(?)",
            date_expr
        ),
        format!(
            "COALESCE(julianday({}), julianday('2000-01-01')) < julianday(?, '+1 day')",
            date_expr
        ),
    ];

    let mut value_expr = spec.value_expr.to_string();
    if spec.requires_attribute {
        let attr_value = config.filters.attribute_value.as_deref();
        if attr_value.is_none_or(|v| v.is_empty() || v.to_lowercase() == "unknown") {
            value_expr = attribute_unknown_expr().to_string();
            select_params.push(SqlParam::Text(
                config.filters.attribute_key.clone().unwrap_or_default(),
            ));
        } else {
            value_expr = attribute_share_expr().to_string();
            select_params.push(SqlParam::Text(
                config.filters.attribute_key.clone().unwrap_or_default(),
            ));
            select_params.push(SqlParam::Text(attr_value.unwrap_or_default().to_string()));
        }
    }

    for w in spec.extra_where {
        if *w == "tr.to_state = ?" {
            where_params.push(SqlParam::Text(
                config.filters.state_key.clone().unwrap_or_default(),
            ));
        }
        where_sql.push((*w).to_string());
    }
    let mut params: Vec<SqlParam> = select_params;
    params.extend(where_params);

    // Common conversation-level filters (only for conversation-anchored
    // FROMs, never for outcomes).
    if spec.applies_conversation_filters() {
        apply_conversation_filters(&config.filters, &mut where_sql, &mut params);
    }

    let sql = safe_sql(&format!(
        "SELECT {dim_expr} AS dv, {group_expr} AS gk, {value_expr} AS v \
         FROM {} {} WHERE {} GROUP BY {group_expr}",
        spec.from,
        joins.join(" "),
        where_sql.join(" AND ")
    ))?;

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Report execution failed: {e}"))?;
    struct RawRow {
        dv: Option<String>,
        gk: rusqlite::types::Value,
        v: Option<f64>,
    }
    let raw: Vec<RawRow> = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(RawRow {
                // JS `String(r.dv)` renders NULL as "null".
                dv: r.get(0)?,
                gk: r.get(1)?,
                v: r.get(2)?,
            })
        })
        .map_err(|e| format!("Report execution failed: {e}"))?
        .filter_map(std::result::Result::ok)
        .collect();

    let mut rows: Vec<ReportBuilderRow> = raw
        .iter()
        .map(|r| ReportBuilderRow {
            dimension_value: r.dv.clone().unwrap_or_else(|| "null".to_string()),
            dimension_label: r.dv.clone().unwrap_or_else(|| "null".to_string()),
            value: r.v.unwrap_or(0.0),
            sample_conversation_ids: Vec::new(),
        })
        .collect();

    // Sample conversations for count metrics (bounded to 3 per row, first 20
    // rows in group-output order — the reference's exact sampling policy).
    if spec.is_conversation_anchored() && spec.value_expr == "COUNT(*)" {
        if config.dimension != ReportDimensionKey::None {
            let up_to = rows.len().min(20);
            for i in 0..up_to {
                rows[i].sample_conversation_ids =
                    sample_conversations(conn, config, spec, &dim, Some(&raw[i].gk));
            }
        } else {
            for row in &mut rows {
                row.sample_conversation_ids = sample_conversations(conn, config, spec, &dim, None);
            }
        }
    }

    let limit = config.limit.map_or(50, |l| l.clamp(1, 200));
    match config.sort {
        ReportSort::MetricAsc => rows.sort_by(|a, b| {
            a.value
                .partial_cmp(&b.value)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.dimension_label.cmp(&b.dimension_label))
        }),
        ReportSort::DimensionAsc => rows.sort_by(|a, b| a.dimension_label.cmp(&b.dimension_label)),
        ReportSort::MetricDesc => rows.sort_by(|a, b| {
            b.value
                .partial_cmp(&a.value)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.dimension_label.cmp(&b.dimension_label))
        }),
    }
    rows.truncate(usize::try_from(limit).unwrap_or(50));
    Ok(rows)
}

/// Run a report (reference `ReportBuilderService.run`). The `Err` string is
/// the exact message for the route's 422 envelope — the reference converts
/// every run failure into a 422 ValidationError, never a 500.
pub fn run_builder_report(
    conn: &Connection,
    config: &ReportBuilderConfig,
) -> std::result::Result<ReportRunOutcome, String> {
    let metric = crate::catalog::reporting::metric_entry(config.metric);
    let dimension = crate::catalog::reporting::dimension_entry(config.dimension);
    let spec = metric_spec(config.metric);
    if (spec.total_only || spec.requires_attribute || spec.requires_state)
        && config.dimension != ReportDimensionKey::None
    {
        return Err(format!(
            "Metric '{}' supports no grouping (total only).",
            config.metric.as_str()
        ));
    }
    if spec.requires_attribute && config.filters.attribute_key.is_none() {
        return Err(format!(
            "Metric '{}' requires an attribute key filter.",
            config.metric.as_str()
        ));
    }
    if spec.requires_state && config.filters.state_key.is_none() {
        return Err(format!(
            "Metric '{}' requires a state key filter.",
            config.metric.as_str()
        ));
    }
    if spec.requires_attribute {
        let key = config.filters.attribute_key.as_deref().unwrap_or_default();
        let in_catalog = crate::catalog::AiAttributeKey::ALL
            .iter()
            .any(|k| k.as_str() == key);
        if !in_catalog {
            return Err(format!(
                "Attribute key '{key}' is not in the closed catalog."
            ));
        }
    }
    if spec.requires_state {
        let key = config.filters.state_key.as_deref().unwrap_or_default();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ticket_states WHERE key = ?1)",
                params![key],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if !exists {
            return Err(format!("State key '{key}' does not exist."));
        }
    }
    // The sla_breached metric reads mailbox_business_hours; make sure the
    // table exists on DBs booted before M034.
    let _ = ensure_mailbox_business_hours(conn);

    let rows = execute_builder_query(conn, config, &spec, &config.date_from, &config.date_to)?;

    let mut comparison_rows: Option<Vec<ReportBuilderRow>> = None;
    let mut comparison_range: Option<ReportDateRange> = None;
    if config.comparison == ReportComparison::PreviousPeriod {
        let from_date = chrono::NaiveDate::parse_from_str(&config.date_from, "%Y-%m-%d")
            .map_err(|_| "Invalid time value".to_string())?;
        let to_date = chrono::NaiveDate::parse_from_str(&config.date_to, "%Y-%m-%d")
            .map_err(|_| "Invalid time value".to_string())?;
        let days = ((to_date - from_date).num_days() + 1).max(1);
        let prev_to = from_date - chrono::Duration::days(1);
        let prev_from = from_date - chrono::Duration::days(days);
        comparison_range = Some(ReportDateRange {
            date_from: prev_from.format("%Y-%m-%d").to_string(),
            date_to: prev_to.format("%Y-%m-%d").to_string(),
        });
        comparison_rows = Some(execute_builder_query(
            conn,
            config,
            &spec,
            &prev_from.format("%Y-%m-%d").to_string(),
            &prev_to.format("%Y-%m-%d").to_string(),
        )?);
    }

    let mut notes: Vec<String> = vec![
        format!("Metric definition: {}", metric.definition),
        format!("Limitations: {}", metric.limitations),
    ];
    if config.comparison == ReportComparison::PreviousPeriod {
        notes.push(
            "Comparison values are differences over an earlier window of the same length; they describe change, not cause."
                .to_string(),
        );
    }
    if spec.requires_attribute && config.filters.attribute_value.is_none() {
        notes.push(
            "A blank attribute value means 'is unknown' (no current attribute row).".to_string(),
        );
    }

    Ok(ReportRunOutcome {
        metric,
        dimension,
        rows,
        comparison_rows,
        comparison_range,
        date_range: ReportDateRange {
            date_from: config.date_from.clone(),
            date_to: config.date_to.clone(),
        },
        notes,
        origin: "local",
    })
}

/// The canonical JSON form of a config (the shape persisted in
/// `report_definitions.config` and echoed back by the save route). Only
/// present filter keys are emitted, like the reference's zod-parsed object.
pub fn config_to_json(config: &ReportBuilderConfig) -> serde_json::Value {
    let filters = {
        let mut f = serde_json::Map::new();
        if let Some(v) = &config.filters.mailbox_local_ids {
            f.insert("mailboxLocalIds".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.channel {
            f.insert("channel".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.tags_any {
            f.insert("tagsAny".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.tags_none {
            f.insert("tagsNone".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.statuses {
            f.insert("statuses".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.assignee_local_ids {
            f.insert("assigneeLocalIds".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.min_priority {
            f.insert("minPriority".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.attribute_key {
            f.insert("attributeKey".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.attribute_value {
            f.insert("attributeValue".into(), serde_json::json!(v));
        }
        if let Some(v) = &config.filters.state_key {
            f.insert("stateKey".into(), serde_json::json!(v));
        }
        f
    };
    let mut v = serde_json::json!({
        "metric": config.metric.as_str(),
        "dimension": config.dimension.as_str(),
        "dateFrom": config.date_from,
        "dateTo": config.date_to,
        "comparison": match config.comparison {
            ReportComparison::None => "none",
            ReportComparison::PreviousPeriod => "previous_period",
        },
        "sort": match config.sort {
            ReportSort::MetricDesc => "metric_desc",
            ReportSort::MetricAsc => "metric_asc",
            ReportSort::DimensionAsc => "dimension_asc",
        },
    });
    if !filters.is_empty() {
        v["filters"] = serde_json::Value::Object(filters);
    }
    if let Some(limit) = config.limit {
        v["limit"] = serde_json::json!(limit);
    }
    v
}

/// List saved report definitions, newest first (reference `listSaved`).
pub fn list_saved_reports(conn: &Connection) -> Result<Vec<SavedReport>> {
    ensure_report_definitions_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, config, created_at, updated_at FROM report_definitions
         ORDER BY created_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let config_text: String = r.get(2)?;
            Ok(SavedReport {
                id: r.get(0)?,
                name: r.get(1)?,
                config: serde_json::from_str(&config_text).unwrap_or(serde_json::Value::Null),
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Save a report definition (reference `saveSaved`). The response carries
/// ISO-8601 timestamps (the reference returns `new Date().toISOString()`
/// even though it stores `datetime('now')` — ported verbatim).
pub fn save_report(
    conn: &Connection,
    name: &str,
    config: &ReportBuilderConfig,
) -> Result<SavedReport> {
    ensure_report_definitions_table(conn)?;
    let config_json =
        serde_json::to_string(&config_to_json(config)).unwrap_or_else(|_| "{}".to_string());
    conn.execute(
        "INSERT INTO report_definitions (name, config) VALUES (?1, ?2)",
        params![name, config_json],
    )?;
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    Ok(SavedReport {
        id: conn.last_insert_rowid(),
        name: name.to_string(),
        config: config_to_json(config),
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Delete a saved report definition. Returns false when the id does not exist.
pub fn delete_saved_report(conn: &Connection, id: i64) -> Result<bool> {
    ensure_report_definitions_table(conn)?;
    let rows = conn.execute("DELETE FROM report_definitions WHERE id = ?1", params![id])?;
    Ok(rows > 0)
}

/// Parse and validate a report config body (the reference's
/// `reportConfigSchema`). Returns `(path, message)` zod-style issue pairs —
/// the route turns them into the reference's joined 422 message.
pub fn parse_report_config(
    body: &serde_json::Value,
) -> std::result::Result<ReportBuilderConfig, Vec<(String, String)>> {
    parse_report_config_inner(body).map_err(|mut issues| {
        issues.truncate(10);
        issues
    })
}

fn enum_error(variants: &[&str], received: &str) -> String {
    let quoted: Vec<String> = variants.iter().map(|v| format!("'{v}'")).collect();
    format!(
        "Invalid enum value. Expected {}, received '{received}'",
        quoted.join(" | ")
    )
}

fn metric_variants() -> Vec<&'static str> {
    ReportMetricKey::ALL.iter().map(|k| k.as_str()).collect()
}

fn dimension_variants() -> Vec<&'static str> {
    ReportDimensionKey::ALL.iter().map(|k| k.as_str()).collect()
}

fn parse_report_config_inner(
    body: &serde_json::Value,
) -> std::result::Result<ReportBuilderConfig, Vec<(String, String)>> {
    let mut issues: Vec<(String, String)> = Vec::new();

    let metric = body.get("metric").and_then(|v| v.as_str());
    let metric = match metric {
        None => {
            issues.push(("metric".to_string(), "Required".to_string()));
            None
        }
        Some(m) => match ReportMetricKey::ALL.iter().find(|k| k.as_str() == m) {
            Some(k) => Some(*k),
            None => {
                issues.push(("metric".to_string(), enum_error(&metric_variants(), m)));
                None
            }
        },
    };

    let dimension = body.get("dimension").and_then(|v| v.as_str());
    let dimension = match dimension {
        None => {
            issues.push(("dimension".to_string(), "Required".to_string()));
            None
        }
        Some(d) => match ReportDimensionKey::ALL.iter().find(|k| k.as_str() == d) {
            Some(k) => Some(*k),
            None => {
                issues.push((
                    "dimension".to_string(),
                    enum_error(&dimension_variants(), d),
                ));
                None
            }
        },
    };

    let date_from = match body.get("dateFrom") {
        None | Some(serde_json::Value::Null) => {
            issues.push(("dateFrom".to_string(), "Required".to_string()));
            String::new()
        }
        Some(serde_json::Value::String(s)) => {
            // ^\d{4}-\d{2}-\d{2}$
            let shape_ok = s.len() == 10
                && s.as_bytes().iter().enumerate().all(|(i, b)| match i {
                    4 | 7 => *b == b'-',
                    _ => b.is_ascii_digit(),
                });
            if shape_ok {
                s.clone()
            } else {
                issues.push(("dateFrom".to_string(), "Invalid".to_string()));
                String::new()
            }
        }
        Some(_) => {
            issues.push((
                "dateFrom".to_string(),
                "Expected string, received non-string".to_string(),
            ));
            String::new()
        }
    };

    let date_to = match body.get("dateTo") {
        None | Some(serde_json::Value::Null) => {
            issues.push(("dateTo".to_string(), "Required".to_string()));
            String::new()
        }
        Some(serde_json::Value::String(s)) => {
            let shape_ok = s.len() == 10
                && s.as_bytes().iter().enumerate().all(|(i, b)| match i {
                    4 | 7 => *b == b'-',
                    _ => b.is_ascii_digit(),
                });
            if shape_ok {
                s.clone()
            } else {
                issues.push(("dateTo".to_string(), "Invalid".to_string()));
                String::new()
            }
        }
        Some(_) => {
            issues.push((
                "dateTo".to_string(),
                "Expected string, received non-string".to_string(),
            ));
            String::new()
        }
    };

    let comparison = match body.get("comparison") {
        None | Some(serde_json::Value::Null) => ReportComparison::None,
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "none" => ReportComparison::None,
            "previous_period" => ReportComparison::PreviousPeriod,
            _ => {
                issues.push((
                    "comparison".to_string(),
                    enum_error(&["none", "previous_period"], s),
                ));
                ReportComparison::None
            }
        },
        Some(_) => {
            issues.push((
                "comparison".to_string(),
                "Expected string, received non-string".to_string(),
            ));
            ReportComparison::None
        }
    };

    let sort = match body.get("sort") {
        None | Some(serde_json::Value::Null) => ReportSort::MetricDesc,
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "metric_desc" => ReportSort::MetricDesc,
            "metric_asc" => ReportSort::MetricAsc,
            "dimension_asc" => ReportSort::DimensionAsc,
            _ => {
                issues.push((
                    "sort".to_string(),
                    enum_error(&["metric_desc", "metric_asc", "dimension_asc"], s),
                ));
                ReportSort::MetricDesc
            }
        },
        Some(_) => {
            issues.push((
                "sort".to_string(),
                "Expected string, received non-string".to_string(),
            ));
            ReportSort::MetricDesc
        }
    };

    let limit = match body.get("limit") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => match v.as_i64() {
            Some(n) if (1..=200).contains(&n) => Some(n),
            Some(n) if n < 1 => {
                issues.push((
                    "limit".to_string(),
                    "Number must be greater than or equal to 1".to_string(),
                ));
                None
            }
            Some(_) => {
                issues.push((
                    "limit".to_string(),
                    "Number must be less than or equal to 200".to_string(),
                ));
                None
            }
            None => {
                issues.push((
                    "limit".to_string(),
                    "Expected number, received non-number".to_string(),
                ));
                None
            }
        },
    };

    let filters = match body.get("filters") {
        None => ReportFilters::default(),
        Some(serde_json::Value::Object(f)) => parse_report_filters(f, &mut issues),
        Some(_) => {
            issues.push((
                "filters".to_string(),
                "Expected object, received non-object".to_string(),
            ));
            ReportFilters::default()
        }
    };

    match (metric, dimension) {
        (Some(metric), Some(dimension)) if issues.is_empty() => Ok(ReportBuilderConfig {
            metric,
            dimension,
            date_from,
            date_to,
            comparison,
            filters,
            sort,
            limit,
        }),
        _ => Err(issues),
    }
}

fn parse_report_filters(
    f: &serde_json::Map<String, serde_json::Value>,
    issues: &mut Vec<(String, String)>,
) -> ReportFilters {
    let mut out = ReportFilters::default();

    // mailboxLocalIds / assigneeLocalIds: arrays of positive ints, max 20.
    for key in ["mailboxLocalIds", "assigneeLocalIds"] {
        if let Some(v) = f.get(key) {
            match v.as_array() {
                Some(arr) if arr.len() > 20 => {
                    issues.push((
                        format!("filters.{key}"),
                        "Array must contain at most 20 item(s)".to_string(),
                    ));
                }
                Some(arr) => {
                    let mut ids = Vec::new();
                    let mut ok = true;
                    for item in arr {
                        match item.as_i64() {
                            Some(n) if n > 0 => ids.push(n),
                            _ => {
                                issues.push((
                                    format!("filters.{key}"),
                                    "Expected positive integer, received non-integer".to_string(),
                                ));
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        if key == "mailboxLocalIds" {
                            out.mailbox_local_ids = Some(ids);
                        } else {
                            out.assignee_local_ids = Some(ids);
                        }
                    }
                }
                None => {
                    issues.push((
                        format!("filters.{key}"),
                        "Expected array, received non-array".to_string(),
                    ));
                }
            }
        }
    }

    // channel: string max 40, nullable.
    if let Some(v) = f.get("channel") {
        match v {
            serde_json::Value::Null => {}
            serde_json::Value::String(s) if s.len() <= 40 => {
                out.channel = Some(s.clone());
            }
            serde_json::Value::String(_) => {
                issues.push((
                    "filters.channel".to_string(),
                    "String must contain at most 40 character(s)".to_string(),
                ));
            }
            _ => {
                issues.push((
                    "filters.channel".to_string(),
                    "Expected string, received non-string".to_string(),
                ));
            }
        }
    }

    // tagsAny / tagsNone: arrays of strings 1..80, max 10. statuses: 1..40, max 10.
    let string_lists = [
        ("tagsAny", 10usize, 80usize, "tagsAny"),
        ("tagsNone", 10, 80, "tagsNone"),
        ("statuses", 10, 40, "statuses"),
    ];
    for (key, max_items, max_len, field) in string_lists {
        if let Some(v) = f.get(key) {
            match v.as_array() {
                Some(arr) if arr.len() > max_items => {
                    issues.push((
                        format!("filters.{field}"),
                        format!("Array must contain at most {max_items} item(s)"),
                    ));
                }
                Some(arr) => {
                    let mut items = Vec::new();
                    let mut ok = true;
                    for item in arr {
                        match item.as_str() {
                            Some(s) if !s.is_empty() && s.len() <= max_len => {
                                items.push(s.to_string());
                            }
                            Some("") => {
                                issues.push((
                                    format!("filters.{field}"),
                                    "String must contain at least 1 character(s)".to_string(),
                                ));
                                ok = false;
                                break;
                            }
                            Some(_) => {
                                issues.push((
                                    format!("filters.{field}"),
                                    format!("String must contain at most {max_len} character(s)"),
                                ));
                                ok = false;
                                break;
                            }
                            None => {
                                issues.push((
                                    format!("filters.{field}"),
                                    "Expected string, received non-string".to_string(),
                                ));
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        match field {
                            "tagsAny" => out.tags_any = Some(items),
                            "tagsNone" => out.tags_none = Some(items),
                            _ => out.statuses = Some(items),
                        }
                    }
                }
                None => {
                    issues.push((
                        format!("filters.{field}"),
                        "Expected array, received non-array".to_string(),
                    ));
                }
            }
        }
    }

    // minPriority: enum, nullable.
    if let Some(v) = f.get("minPriority") {
        match v {
            serde_json::Value::Null => {}
            serde_json::Value::String(s)
                if ["low", "normal", "high", "urgent"].contains(&s.as_str()) =>
            {
                out.min_priority = Some(s.clone());
            }
            serde_json::Value::String(s) => {
                issues.push((
                    "filters.minPriority".to_string(),
                    enum_error(&["low", "normal", "high", "urgent"], s),
                ));
            }
            _ => {
                issues.push((
                    "filters.minPriority".to_string(),
                    "Expected string, received non-string".to_string(),
                ));
            }
        }
    }

    // attributeKey (<=60) / attributeValue (<=200) / stateKey (<=80): nullable strings.
    let nullable_strings = [
        ("attributeKey", 60usize, "attribute_key"),
        ("attributeValue", 200, "attribute_value"),
        ("stateKey", 80, "state_key"),
    ];
    for (key, max_len, field) in nullable_strings {
        if let Some(v) = f.get(key) {
            match v {
                serde_json::Value::Null => {}
                serde_json::Value::String(s) if s.len() <= max_len => match field {
                    "attribute_key" => out.attribute_key = Some(s.clone()),
                    "attribute_value" => out.attribute_value = Some(s.clone()),
                    _ => out.state_key = Some(s.clone()),
                },
                serde_json::Value::String(_) => {
                    issues.push((
                        format!("filters.{key}"),
                        format!("String must contain at most {max_len} character(s)"),
                    ));
                }
                _ => {
                    issues.push((
                        format!("filters.{key}"),
                        "Expected string, received non-string".to_string(),
                    ));
                }
            }
        }
    }

    out
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

    // Escalated (has a current escalation_signal AI attribute or escalation_intent interaction signal).
    // ai_attributes stores local conversation ids + reference booleans
    // ('true'/'false', current row = superseded_at IS NULL).
    let escalated: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ai_attributes
             WHERE conversation_id = (SELECT id FROM conversations WHERE remote_id = ?1)
               AND attribute = 'escalation_signal'
               AND value = 'true'
               AND superseded_at IS NULL)",
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

/// The friction findings recorded for one conversation
/// (`GET /api/friction/:conversationId`). Reads the `friction_scores` rows
/// (M020: id, conversation_id, effort_score, factors_json, created_at) and
/// serializes each row snake_case, parsing the factors JSON. Conversations
/// without a recorded score yield an empty list.
pub fn friction_findings(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, effort_score, factors_json, created_at
         FROM friction_scores
         WHERE conversation_id = ?1
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![conversation_id], |r| {
            let factors_text: Option<String> = r.get(3)?;
            Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "conversation_id": r.get::<_, i64>(1)?,
                "effort_score": r.get::<_, f64>(2)?,
                "factors": factors_text
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()),
                "created_at": r.get::<_, String>(4)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── M8-T05: Support health (operational facts, NOT a 0-100 score) ──────
//
// Per spec section 58: no aggregate 0-100 health score.
// Instead, expose the raw operational facts with definitions + evidence.
// The UI shows these as labeled metrics, not a single number.

/// Operational health facts — individual metrics with definitions.
/// Per spec section 58: no aggregate score; show operational facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthFacts {
    /// Total conversations in the period.
    pub total_conversations: u32,
    /// SLA breach count (from sla_breaches table).
    pub sla_breach_count: u32,
    /// SLA breach rate = sla_breach_count / total_conversations.
    /// Definition: "Fraction of conversations with at least one SLA breach."
    pub sla_breach_rate: Option<f64>,
    /// High-friction conversation count.
    pub high_friction_count: u32,
    /// High-friction rate = high_friction / total.
    /// Definition: "Fraction of conversations with effort_score >= 0.6."
    pub high_friction_rate: Option<f64>,
    /// Average first response time in minutes (None if no data).
    /// Definition: "Mean time from conversation creation to first agent response."
    pub avg_first_response_minutes: Option<f64>,
    /// Resolution rate = closed / total.
    /// Definition: "Fraction of conversations that have been closed."
    pub resolution_rate: Option<f64>,
}

/// Get operational health facts. Per spec section 58: show operational facts
/// with definitions, NOT a 0-100 aggregate score.
pub fn get_health_facts(conn: &Connection, days_back: u32) -> Result<HealthFacts> {
    let metrics = get_dashboard_metrics(conn, None, days_back)?;
    let high_friction = count_high_friction(conn).unwrap_or(0);

    let total = metrics.total_conversations;
    let sla_breach_rate = if total > 0 {
        Some(metrics.sla_breach_count as f64 / total as f64)
    } else {
        None
    };
    let high_friction_rate = if total > 0 {
        Some(high_friction as f64 / total as f64)
    } else {
        None
    };
    let resolution_rate = if total > 0 {
        Some(metrics.closed_conversations as f64 / total as f64)
    } else {
        None
    };

    Ok(HealthFacts {
        total_conversations: total,
        sla_breach_count: metrics.sla_breach_count,
        sla_breach_rate,
        high_friction_count: high_friction,
        high_friction_rate,
        avg_first_response_minutes: metrics.avg_first_response_minutes,
        resolution_rate,
    })
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

/// Get neighbors of a node (both directions of the stored human edges
/// touching (kind, local mirror id)) — the interim reader the graph routes
/// use until the derived-edge layer lands (GR-01/GR-03). Kept for the
/// module's public surface; the wire building lives in support_graph.
pub fn get_graph_neighbors(
    conn: &Connection,
    kind: &str,
    local_id: i64,
) -> Result<Vec<crate::support_graph::GraphNodeRef>> {
    let Some(kind) = validate_graph_node_kind(kind).ok() else {
        return Ok(Vec::new());
    };
    let edges = crate::support_graph::human_edges_touching(conn, kind, local_id)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for edge in &edges {
        for side in ["source", "target"] {
            if let Some(v) = edge.get(side) {
                let key = serde_json::to_string(v).unwrap_or_default();
                if seen.insert(key) {
                    if let Ok(r) =
                        serde_json::from_value::<crate::support_graph::GraphNodeRef>(v.clone())
                    {
                        if r.kind != kind.as_str() || r.local_id != local_id {
                            out.push(r);
                        }
                    }
                }
            }
        }
    }
    Ok(out)
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

/// List all graph nodes (most recent first).
pub fn list_graph_nodes(conn: &Connection, limit: Option<u32>) -> Result<Vec<GraphNode>> {
    let limit = limit.unwrap_or(100).min(500) as i64;
    let mut stmt = conn.prepare(
        "SELECT id, kind, entity_id, label, properties_json, created_at
         FROM graph_nodes ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok(GraphNode {
            id: r.get(0)?,
            kind: r.get(1)?,
            entity_id: r.get(2)?,
            label: r.get(3)?,
            properties: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.into())
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
        crate::ai_attributes::apply_m033(&conn).unwrap();
        apply_m011_to_m013(&conn).unwrap();
        apply_m014(&conn).unwrap();
        apply_m015_to_m019(&conn).unwrap();
        apply_m020_to_m022(&conn).unwrap();
        crate::outreach::apply_m023_to_m025(&conn).unwrap();
        crate::inbox::apply_m028(&conn).unwrap();
        crate::conversation_ops::apply_m030(&conn).unwrap();
        crate::outreach::apply_m031(&conn).unwrap();
        crate::ticket_states::apply_m032(&conn).unwrap();
        apply_m034(&conn).unwrap();
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
        // GR-02: graph_edges is the reference migration-016 human-edge
        // shape (kind/local_id keyed, 5-relation CHECK), not the legacy
        // graph_nodes-surrogate model.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(graph_edges)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for col in [
            "source_kind",
            "source_local_id",
            "target_kind",
            "target_local_id",
            "relation",
            "note",
            "created_by_user_local_id",
            "created_at",
            "provenance",
        ] {
            assert!(cols.iter().any(|c| c == col), "graph_edges.{col} missing");
        }
        assert!(
            !cols.iter().any(|c| c == "source_id"),
            "legacy shape leaked"
        );
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

    // ---- M8-T05: Health facts (operational, not aggregate score) ----------

    #[test]
    fn health_facts_empty_db() {
        let conn = fresh_db();
        let facts = get_health_facts(&conn, 7).unwrap();
        assert_eq!(facts.total_conversations, 0);
        assert!(facts.sla_breach_rate.is_none());
        assert!(facts.resolution_rate.is_none());
    }

    #[test]
    fn health_facts_with_data() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "closed", 101, 2001);
        insert_conversation(&conn, 1002, "active", 101, 2002);
        let facts = get_health_facts(&conn, 7).unwrap();
        assert_eq!(facts.total_conversations, 2);
        assert_eq!(facts.sla_breach_count, 0);
        assert!(facts.sla_breach_rate.is_some());
        assert!((facts.sla_breach_rate.unwrap() - 0.0).abs() < 1e-6);
        assert!((facts.resolution_rate.unwrap() - 0.5).abs() < 1e-6);
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
        // Human edges now address mirror rows by (kind, local id), so the
        // neighbor reader needs real mirror rows, not graph_nodes surrogates
        // (GR-02: the surrogate edge model is gone).
        conn.execute(
            "INSERT INTO conversations (remote_id, number, subject, status, mailbox_id, customer_id)
             VALUES (1001, 1001, 'Bug', 'active', 1, 1)",
            [],
        )
        .unwrap();
        let conv_id: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 1001",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO known_issues (id, name, status) VALUES (1, 'Login bug', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_edges (source_kind, source_local_id, target_kind, target_local_id, relation, provenance)
             VALUES ('conversation', ?1, 'known_issue', 1, 'mentions', 'human_local')",
            params![conv_id],
        )
        .unwrap();

        let neighbors = get_graph_neighbors(&conn, "conversation", conv_id).unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].kind, "known_issue");
        let incoming = get_graph_neighbors(&conn, "known_issue", 1).unwrap();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].kind, "conversation");
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
    fn health_facts_serializes() {
        let f = HealthFacts {
            total_conversations: 10,
            sla_breach_count: 2,
            sla_breach_rate: Some(0.2),
            high_friction_count: 1,
            high_friction_rate: Some(0.1),
            avg_first_response_minutes: Some(45.0),
            resolution_rate: Some(0.8),
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("\"sla_breach_rate\":0.2"));
        assert!(s.contains("\"total_conversations\":10"));
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
