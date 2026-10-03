//! Intelligence features — known issues, clusters, Issue Radar, incidents,
//! SLA, knowledge docs (M7-T02 through M7-T07).
//!
//! Per spec M7: "Issue Radar, known issues and clusters, incidents and impact,
//! SLA, knowledge, Docs, freshness and gaps."

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{IncidentSeverity, IncidentSource, IncidentStatus};
use crate::error::Result;

/// Migrations M015–M019 combined.
pub const M015_TO_M019_SQL: &str = r#"
    -- M015: known_issues + known_issue_links
    CREATE TABLE IF NOT EXISTS known_issues (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        name        TEXT NOT NULL,
        status      TEXT NOT NULL DEFAULT 'active',
        description TEXT,
        created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE TABLE IF NOT EXISTS known_issue_links (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        known_issue_id  INTEGER NOT NULL REFERENCES known_issues (id) ON DELETE CASCADE,
        conversation_id INTEGER NOT NULL,
        link_type       TEXT NOT NULL DEFAULT 'related'
    );
    CREATE INDEX IF NOT EXISTS idx_known_issue_links
        ON known_issue_links (known_issue_id, conversation_id);

    -- M016: issue_clusters + issue_cluster_members
    CREATE TABLE IF NOT EXISTS issue_clusters (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        name            TEXT NOT NULL,
        conversation_count INTEGER NOT NULL DEFAULT 0,
        first_seen_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        last_seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        status          TEXT NOT NULL DEFAULT 'active'
    );
    CREATE TABLE IF NOT EXISTS issue_cluster_members (
        cluster_id      INTEGER NOT NULL REFERENCES issue_clusters (id) ON DELETE CASCADE,
        conversation_id INTEGER NOT NULL,
        PRIMARY KEY (cluster_id, conversation_id)
    );

    -- M017: incidents
    CREATE TABLE IF NOT EXISTS incidents (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        known_issue_id  INTEGER REFERENCES known_issues (id),
        status          TEXT NOT NULL DEFAULT 'investigating',
        severity        TEXT NOT NULL DEFAULT 'sev4',
        source          TEXT NOT NULL DEFAULT 'manual',
        description     TEXT,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        resolved_at     TEXT
    );

    -- M018: sla_configs + sla_breaches
    CREATE TABLE IF NOT EXISTS sla_configs (
        mailbox_id          INTEGER PRIMARY KEY,
        first_response_hours REAL NOT NULL DEFAULT 24.0,
        resolution_hours     REAL NOT NULL DEFAULT 72.0
    );
    CREATE TABLE IF NOT EXISTS sla_breaches (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        breach_type     TEXT NOT NULL,
        breached_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );

    -- M019: knowledge_doc_freshness + knowledge_gaps
    CREATE TABLE IF NOT EXISTS knowledge_doc_freshness (
        doc_id          INTEGER PRIMARY KEY,
        last_synced_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        content_hash    TEXT,
        stale_at        TEXT
    );
    CREATE TABLE IF NOT EXISTS knowledge_gaps (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        query_text      TEXT NOT NULL,
        hit_count       INTEGER NOT NULL DEFAULT 1,
        last_seen_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_knowledge_gaps_query
        ON knowledge_gaps (query_text);

    UPDATE app_state SET schema_version = 19 WHERE id = 1;
"#;

/// Apply M015–M019 migrations. Idempotent.
pub fn apply_m015_to_m019(conn: &Connection) -> Result<()> {
    conn.execute_batch(M015_TO_M019_SQL)?;
    Ok(())
}

// ─── M7-T02: Known issues ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownIssue {
    pub id: Option<i64>,
    pub name: String,
    pub status: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub fn create_known_issue(conn: &Connection, name: &str, description: Option<&str>) -> Result<i64> {
    conn.execute(
        "INSERT INTO known_issues (name, description) VALUES (?1, ?2)",
        params![name, description],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_known_issue_status(conn: &Connection, id: i64, status: &str) -> Result<bool> {
    let rows = conn.execute(
        "UPDATE known_issues SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
        params![status, id],
    )?;
    Ok(rows > 0)
}

pub fn add_issue_link(
    conn: &Connection,
    known_issue_id: i64,
    conversation_id: i64,
    link_type: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (?1, ?2, ?3)",
        params![known_issue_id, conversation_id, link_type],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_known_issues(
    conn: &Connection,
    status_filter: Option<&str>,
) -> Result<Vec<KnownIssue>> {
    let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(st) = status_filter {
        (
            "SELECT id, name, status, description, created_at, updated_at
             FROM known_issues WHERE status = ?1 ORDER BY id DESC",
            vec![Box::new(st.to_string())],
        )
    } else {
        (
            "SELECT id, name, status, description, created_at, updated_at
             FROM known_issues ORDER BY id DESC",
            vec![],
        )
    };
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(KnownIssue {
                id: r.get(0)?,
                name: r.get(1)?,
                status: r.get(2)?,
                description: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── M7-T03: Issue clusters ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueCluster {
    pub id: Option<i64>,
    pub name: String,
    pub conversation_count: i64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub status: String,
}

pub fn create_issue_cluster(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO issue_clusters (name) VALUES (?1)",
        params![name],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn add_cluster_member(
    conn: &Connection,
    cluster_id: i64,
    conversation_id: i64,
) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO issue_cluster_members (cluster_id, conversation_id) VALUES (?1, ?2)",
        params![cluster_id, conversation_id],
    )?;
    conn.execute(
        "UPDATE issue_clusters SET conversation_count = (SELECT COUNT(*) FROM issue_cluster_members WHERE cluster_id = ?1),
         last_seen_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
        params![cluster_id],
    )?;
    Ok(true)
}

pub fn list_issue_clusters(conn: &Connection) -> Result<Vec<IssueCluster>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, conversation_count, first_seen_at, last_seen_at, status
         FROM issue_clusters ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(IssueCluster {
                id: r.get(0)?,
                name: r.get(1)?,
                conversation_count: r.get(2)?,
                first_seen_at: r.get(3)?,
                last_seen_at: r.get(4)?,
                status: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ─── M7-T04: Issue Radar ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadarSnapshot {
    pub active_known_issues: u32,
    pub active_clusters: u32,
    pub active_incidents: u32,
}

pub fn get_radar_snapshot(conn: &Connection) -> Result<RadarSnapshot> {
    let active_issues: i64 = conn.query_row(
        "SELECT COUNT(*) FROM known_issues WHERE status = 'active'",
        [],
        |r| r.get(0),
    )?;
    let active_clusters: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issue_clusters WHERE status = 'active'",
        [],
        |r| r.get(0),
    )?;
    let active_incidents: i64 = conn.query_row(
        "SELECT COUNT(*) FROM incidents WHERE status != 'resolved'",
        [],
        |r| r.get(0),
    )?;
    Ok(RadarSnapshot {
        active_known_issues: u32::try_from(active_issues).unwrap_or(0),
        active_clusters: u32::try_from(active_clusters).unwrap_or(0),
        active_incidents: u32::try_from(active_incidents).unwrap_or(0),
    })
}

// ─── M7-T05: Incidents ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub id: Option<i64>,
    pub known_issue_id: Option<i64>,
    pub status: String,
    pub severity: String,
    pub source: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
}

pub fn promote_to_incident(
    conn: &Connection,
    known_issue_id: Option<i64>,
    severity: IncidentSeverity,
    source: IncidentSource,
    description: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO incidents (known_issue_id, severity, source, description)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            known_issue_id,
            severity_as_str(severity),
            source_as_str(source),
            description
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_incident_status(conn: &Connection, id: i64, status: IncidentStatus) -> Result<bool> {
    let sql = "UPDATE incidents SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
         resolved_at = CASE WHEN ?1 = 'resolved' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE resolved_at END
         WHERE id = ?2";
    let rows = conn.execute(sql, params![status_as_str(status), id])?;
    Ok(rows > 0)
}

pub fn list_incidents(
    conn: &Connection,
    status_filter: Option<IncidentStatus>,
) -> Result<Vec<Incident>> {
    let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(st) = status_filter {
        (
            "SELECT id, known_issue_id, status, severity, source, description, created_at, updated_at, resolved_at
             FROM incidents WHERE status = ?1 ORDER BY id DESC",
            vec![Box::new(status_as_str(st).to_string())],
        )
    } else {
        (
            "SELECT id, known_issue_id, status, severity, source, description, created_at, updated_at, resolved_at
             FROM incidents ORDER BY id DESC",
            vec![],
        )
    };
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |r| {
            Ok(Incident {
                id: r.get(0)?,
                known_issue_id: r.get(1)?,
                status: r.get(2)?,
                severity: r.get(3)?,
                source: r.get(4)?,
                description: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
                resolved_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn severity_as_str(s: IncidentSeverity) -> &'static str {
    match s {
        IncidentSeverity::Sev1 => "sev1",
        IncidentSeverity::Sev2 => "sev2",
        IncidentSeverity::Sev3 => "sev3",
        IncidentSeverity::Sev4 => "sev4",
    }
}

fn source_as_str(s: IncidentSource) -> &'static str {
    match s {
        IncidentSource::Manual => "manual",
        IncidentSource::Cluster => "cluster",
        IncidentSource::KnownIssue => "known_issue",
    }
}

fn status_as_str(s: IncidentStatus) -> &'static str {
    match s {
        IncidentStatus::Investigating => "investigating",
        IncidentStatus::Identified => "identified",
        IncidentStatus::FixInProgress => "fix_in_progress",
        IncidentStatus::Monitoring => "monitoring",
        IncidentStatus::Resolved => "resolved",
    }
}

// ─── M7-T06: SLA ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlaConfig {
    pub mailbox_id: i64,
    pub first_response_hours: f64,
    pub resolution_hours: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SlaStatus {
    Ok,
    AtRisk,
    Breached,
}

pub fn set_sla_config(
    conn: &Connection,
    mailbox_id: i64,
    first_response_hours: f64,
    resolution_hours: f64,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO sla_configs (mailbox_id, first_response_hours, resolution_hours)
         VALUES (?1, ?2, ?3)",
        params![mailbox_id, first_response_hours, resolution_hours],
    )?;
    Ok(())
}

pub fn get_sla_config(conn: &Connection, mailbox_id: i64) -> Result<Option<SlaConfig>> {
    let row: Option<(i64, f64, f64)> = conn
        .query_row(
            "SELECT mailbox_id, first_response_hours, resolution_hours FROM sla_configs WHERE mailbox_id = ?1",
            params![mailbox_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    match row {
        None => Ok(None),
        Some((id, fr, res)) => Ok(Some(SlaConfig {
            mailbox_id: id,
            first_response_hours: fr,
            resolution_hours: res,
        })),
    }
}

pub fn record_sla_breach(
    conn: &Connection,
    conversation_id: i64,
    breach_type: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO sla_breaches (conversation_id, breach_type) VALUES (?1, ?2)",
        params![conversation_id, breach_type],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn count_sla_breaches(conn: &Connection) -> Result<u32> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM sla_breaches", [], |r| r.get(0))?;
    Ok(u32::try_from(count).unwrap_or(0))
}

// ─── M7-T07: Knowledge docs — freshness + gaps ──────────────────────────

pub fn record_knowledge_gap(conn: &Connection, query_text: &str) -> Result<()> {
    // Manual upsert: try to insert, if exists, update the count.
    let existing: Option<i64> = conn
        .query_row(
            "SELECT hit_count FROM knowledge_gaps WHERE query_text = ?1",
            params![query_text],
            |r| r.get(0),
        )
        .ok();
    if existing.is_some() {
        conn.execute(
            "UPDATE knowledge_gaps SET hit_count = hit_count + 1, last_seen_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE query_text = ?1",
            params![query_text],
        )?;
    } else {
        conn.execute(
            "INSERT INTO knowledge_gaps (query_text, hit_count, last_seen_at)
             VALUES (?1, 1, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            params![query_text],
        )?;
    }
    Ok(())
}

pub fn list_knowledge_gaps(conn: &Connection, limit: u32) -> Result<Vec<(String, i64)>> {
    let limit = limit.clamp(1, 200);
    let mut stmt = conn.prepare(
        "SELECT query_text, hit_count FROM knowledge_gaps ORDER BY hit_count DESC, last_seen_at DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn update_doc_freshness(
    conn: &Connection,
    doc_id: i64,
    content_hash: Option<&str>,
    stale: bool,
) -> Result<()> {
    let stale_at = if stale {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    } else {
        "NULL"
    };
    let sql = format!(
        "INSERT OR REPLACE INTO knowledge_doc_freshness (doc_id, last_synced_at, content_hash, stale_at)
         VALUES (?1, strftime('%Y-%m-%dT%H:%M:%fZ','now'), ?2, {stale_at})"
    );
    conn.execute(&sql, params![doc_id, content_hash])?;
    Ok(())
}

pub fn count_stale_docs(conn: &Connection) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM knowledge_doc_freshness WHERE stale_at IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(0))
}

// ─── M035: reference-shaped incident workspace + gap candidates ──────────
//
// The v1.x port created only the bare `incidents` table (M017) while its
// HTTP routes referenced `incident_conversations` / `incident_timeline`
// without ever creating them. This batch brings the reference's migration
// 014 (m4_intelligence_workspace) link/ref/related tables into existence,
// adds the reference's `title`/`code`/`known_issue_id` columns, and creates
// the v2.1.0 knowledge-gap candidate pipeline table, the v1.x interaction
// override table, the automation run log, and the memory `source` column.

/// Apply the M035 batch. Idempotent.
pub fn apply_m035(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS incident_conversations (
            incident_id     INTEGER NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
            conversation_id INTEGER NOT NULL,
            linked_by       TEXT NOT NULL DEFAULT 'human',
            linked_at       TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (incident_id, conversation_id)
        );
        CREATE INDEX IF NOT EXISTS idx_incident_conversations_conversation
            ON incident_conversations (conversation_id);

        CREATE TABLE IF NOT EXISTS incident_related (
            incident_id     INTEGER NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
            target_kind     TEXT NOT NULL,
            target_local_id INTEGER NOT NULL,
            note            TEXT,
            linked_at       TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (incident_id, target_kind, target_local_id)
        );

        CREATE TABLE IF NOT EXISTS incident_refs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            incident_id INTEGER NOT NULL REFERENCES incidents (id) ON DELETE CASCADE,
            system      TEXT NOT NULL,
            reference   TEXT NOT NULL,
            url         TEXT,
            title       TEXT,
            status      TEXT,
            notes       TEXT,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_incident_refs_incident
            ON incident_refs (incident_id);

        -- v2.1.0 (M5, plan Phase 26) knowledge-gap candidate pipeline.
        -- `status` is the task-specified port vocabulary: an undecided
        -- candidate is 'open' (the reference calls it 'candidate').
        CREATE TABLE IF NOT EXISTS knowledge_gap_candidates (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            query_text       TEXT NOT NULL,
            occurrence_count INTEGER NOT NULL DEFAULT 1,
            kind             TEXT,
            status           TEXT NOT NULL DEFAULT 'open'
                CHECK (status IN ('open','approved','rejected')),
            decision_note    TEXT,
            decided_at       TEXT,
            created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );

        -- v1.x interaction overrides (spec #22, #45, #56): a human
        -- response-preference override takes precedence over AI inference.
        CREATE TABLE IF NOT EXISTS interaction_overrides (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            customer_id INTEGER NOT NULL,
            field       TEXT NOT NULL,
            value       TEXT NOT NULL,
            reason      TEXT,
            created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            UNIQUE (customer_id, field)
        );

        -- Automation run log (reference engine.record → automation_runs).
        CREATE TABLE IF NOT EXISTS automation_runs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            rule_id         INTEGER NOT NULL REFERENCES automation_rules (id) ON DELETE CASCADE,
            conversation_id INTEGER,
            triggered_by    TEXT NOT NULL DEFAULT 'manual',
            outcome         TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_automation_runs_rule
            ON automation_runs (rule_id);",
    )?;
    // Reference 014 columns the legacy table lacked (PRAGMA-guarded).
    add_column_if_missing(conn, "incidents", "title", "TEXT")?;
    add_column_if_missing(conn, "incidents", "code", "TEXT")?;
    add_column_if_missing(conn, "incidents", "owner_user_local_id", "INTEGER")?;
    add_column_if_missing(conn, "incidents", "product", "TEXT")?;
    add_column_if_missing(conn, "incidents", "feature", "TEXT")?;
    add_column_if_missing(conn, "incidents", "internal_explanation", "TEXT")?;
    add_column_if_missing(conn, "incidents", "customer_safe_explanation", "TEXT")?;
    add_column_if_missing(conn, "incidents", "known_cause", "TEXT")?;
    add_column_if_missing(conn, "incidents", "workaround", "TEXT")?;
    add_column_if_missing(conn, "incidents", "resolution", "TEXT")?;
    add_column_if_missing(conn, "incidents", "started_at", "TEXT")?;
    add_column_if_missing(conn, "issue_clusters", "known_issue_id", "INTEGER")?;
    // Reference 003 customer_memories.source ('ai' default; 'human' rows
    // are the only ones a human may delete).
    add_column_if_missing(
        conn,
        "customer_memory",
        "source",
        "TEXT NOT NULL DEFAULT 'ai'",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 35 WHERE id = 1", []);
    Ok(())
}

/// `ALTER TABLE ... ADD COLUMN` guarded by a PRAGMA table_info check
/// (the same idempotency pattern as M032 / reference migration 016).
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let cols: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !cols.iter().any(|c| c == column) {
        let _ = conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        );
    }
    Ok(())
}

/// Reference incidentRepo.nextCode: `INC-###` — max numeric suffix + 1,
/// zero-padded to 3 (INC-001 when no coded incidents exist yet).
fn next_incident_code(conn: &Connection) -> String {
    let maxn: Option<i64> = conn
        .query_row(
            "SELECT MAX(CAST(substr(code, 5) AS INTEGER)) AS maxn FROM incidents
             WHERE code LIKE 'INC-%' AND length(code) >= 5
               AND substr(code, 5) GLOB '[0-9]*'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(None);
    format!("INC-{:03}", maxn.unwrap_or(0) + 1)
}

/// Reference incidentRepo.create, reduced to the columns the port stores:
/// generates the INC code, inserts the row, and links every conversation
/// id (`linked_by = 'human'`, INSERT OR IGNORE). Returns the new row id.
#[allow(clippy::too_many_arguments)]
pub fn create_incident_from_source(
    conn: &Connection,
    known_issue_id: Option<i64>,
    severity: &str,
    status: &str,
    source: &str,
    title: &str,
    description: Option<&str>,
    conversation_ids: &[i64],
) -> Result<i64> {
    let code = next_incident_code(conn);
    let resolved_at = if status == "resolved" {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    } else {
        "NULL"
    };
    let sql = format!(
        "INSERT INTO incidents (known_issue_id, status, severity, source, description, title, code, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, {resolved_at})"
    );
    conn.execute(
        &sql,
        params![
            known_issue_id,
            status,
            severity,
            source,
            description,
            title,
            code
        ],
    )?;
    let id = conn.last_insert_rowid();
    for conv_id in conversation_ids {
        conn.execute(
            "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id, linked_by)
             VALUES (?1, ?2, 'human')",
            params![id, conv_id],
        )?;
    }
    Ok(id)
}

/// A fully-specified manual incident (the reference incidentCreateSchema
/// payload, already defaulted + validated by the route).
#[derive(Debug, Clone)]
pub struct ManualIncident {
    /// Trimmed title, 1..200 chars.
    pub title: String,
    /// One of the 5 reference statuses.
    pub status: String,
    /// One of the 4 reference severities.
    pub severity: String,
    /// Optional owner (users.id).
    pub owner_user_local_id: Option<i64>,
    /// Optional product label.
    pub product: Option<String>,
    /// Optional feature label.
    pub feature: Option<String>,
    /// Optional public description (<= 4000).
    pub description: Option<String>,
    /// Optional internal explanation (<= 8000).
    pub internal_explanation: Option<String>,
    /// Optional customer-safe explanation (<= 8000).
    pub customer_safe_explanation: Option<String>,
    /// Optional known cause (<= 4000).
    pub known_cause: Option<String>,
    /// Optional workaround (<= 4000).
    pub workaround: Option<String>,
    /// Optional resolution (<= 4000).
    pub resolution: Option<String>,
    /// Optional ISO start timestamp.
    pub started_at: Option<String>,
    /// Conversation ids to link (already validated against the mirror).
    pub conversation_ids: Vec<i64>,
}

/// Create a manual incident with the full reference 014 field set
/// (reference incidentService.create with source='manual').
pub fn create_manual_incident(conn: &Connection, inc: &ManualIncident) -> Result<i64> {
    let code = next_incident_code(conn);
    let resolved_at = if inc.status == "resolved" {
        "strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    } else {
        "NULL"
    };
    let sql = format!(
        "INSERT INTO incidents (known_issue_id, status, severity, source, title, code, resolved_at,
             owner_user_local_id, product, feature, description, internal_explanation,
             customer_safe_explanation, known_cause, workaround, resolution, started_at)
         VALUES (NULL, ?1, ?2, 'manual', ?3, ?4, {resolved_at}, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
    );
    conn.execute(
        &sql,
        params![
            inc.status,
            inc.severity,
            inc.title,
            code,
            inc.owner_user_local_id,
            inc.product,
            inc.feature,
            inc.description,
            inc.internal_explanation,
            inc.customer_safe_explanation,
            inc.known_cause,
            inc.workaround,
            inc.resolution,
            inc.started_at,
        ],
    )?;
    let id = conn.last_insert_rowid();
    for conv_id in &inc.conversation_ids {
        conn.execute(
            "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id, linked_by)
             VALUES (?1, ?2, 'human')",
            params![id, conv_id],
        )?;
    }
    Ok(id)
}

/// The full incident row as JSON (id, code, title, known_issue_id, status,
/// severity, source, description, created_at, updated_at, resolved_at) —
/// the reference `repo().get(id)` record shape.
pub fn incident_row_json(conn: &Connection, id: i64) -> Option<Value> {
    conn.query_row(
        "SELECT id, code, title, known_issue_id, status, severity, source,
                description, created_at, updated_at, resolved_at
         FROM incidents WHERE id = ?1",
        params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "code": r.get::<_, Option<String>>(1)?,
                "title": r.get::<_, Option<String>>(2)?,
                "known_issue_id": r.get::<_, Option<i64>>(3)?,
                "status": r.get::<_, String>(4)?,
                "severity": r.get::<_, String>(5)?,
                "source": r.get::<_, String>(6)?,
                "description": r.get::<_, Option<String>>(7)?,
                "created_at": r.get::<_, String>(8)?,
                "updated_at": r.get::<_, String>(9)?,
                "resolved_at": r.get::<_, Option<String>>(10)?,
            }))
        },
    )
    .ok()
}

/// Reference incidentRepo.linkConversation (minus the caller-side
/// existence checks): INSERT OR IGNORE + bump `updated_at`.
/// Returns whether a new link was created.
pub fn link_incident_conversation(
    conn: &Connection,
    incident_id: i64,
    conversation_id: i64,
    linked_by: &str,
) -> Result<bool> {
    let created = conn.execute(
        "INSERT OR IGNORE INTO incident_conversations (incident_id, conversation_id, linked_by)
         VALUES (?1, ?2, ?3)",
        params![incident_id, conversation_id, linked_by],
    )? > 0;
    if created {
        conn.execute(
            "UPDATE incidents SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![incident_id],
        )?;
    }
    Ok(created)
}

/// Reference incidentRepo.unlinkConversation: DELETE + bump `updated_at`.
/// Returns whether a link was removed.
pub fn unlink_incident_conversation(
    conn: &Connection,
    incident_id: i64,
    conversation_id: i64,
) -> Result<bool> {
    let removed = conn.execute(
        "DELETE FROM incident_conversations WHERE incident_id = ?1 AND conversation_id = ?2",
        params![incident_id, conversation_id],
    )? > 0;
    if removed {
        conn.execute(
            "UPDATE incidents SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![incident_id],
        )?;
    }
    Ok(removed)
}

/// Reference incidentRepo.deleteRef. Returns whether a ref was removed.
pub fn delete_incident_ref(conn: &Connection, incident_id: i64, ref_id: i64) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM incident_refs WHERE incident_id = ?1 AND id = ?2",
        params![incident_id, ref_id],
    )? > 0)
}

/// Reference incidentRepo.addRelated (INSERT OR IGNORE).
pub fn add_incident_related(
    conn: &Connection,
    incident_id: i64,
    target_kind: &str,
    target_local_id: i64,
    note: Option<&str>,
) -> Result<bool> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO incident_related (incident_id, target_kind, target_local_id, note)
         VALUES (?1, ?2, ?3, ?4)",
        params![incident_id, target_kind, target_local_id, note],
    )? > 0)
}

/// Reference issueRepo.getCluster().conversation_ids — every conversation
/// id linked to the cluster, oldest link first.
pub fn cluster_conversation_ids(conn: &Connection, cluster_id: i64) -> Vec<i64> {
    conn.prepare(
        "SELECT conversation_id FROM issue_cluster_members WHERE cluster_id = ?1 ORDER BY rowid",
    )
    .map(|mut stmt| {
        stmt.query_map(params![cluster_id], |r| r.get::<_, i64>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Reference `SELECT conversation_id FROM known_issue_conversations WHERE
/// known_issue_id = ?` — the port stores the same links in
/// `known_issue_links` (M015).
pub fn known_issue_conversation_ids(conn: &Connection, known_issue_id: i64) -> Vec<i64> {
    conn.prepare("SELECT conversation_id FROM known_issue_links WHERE known_issue_id = ?1")
        .map(|mut stmt| {
            stmt.query_map(params![known_issue_id], |r| r.get::<_, i64>(0))
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
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
        apply_m015_to_m019(&conn).unwrap();
        conn
    }

    // ---- M015–M019 migrations ---------------------------------------------

    #[test]
    fn m015_creates_known_issues_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM known_issues", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m016_creates_issue_clusters_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_clusters", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m017_creates_incidents_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM incidents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m018_creates_sla_tables() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sla_configs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m019_creates_knowledge_tables() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM knowledge_gaps", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m015_to_m019_is_idempotent() {
        let conn = fresh_db();
        apply_m015_to_m019(&conn).unwrap();
    }

    // ---- M7-T02: Known issues CRUD ----------------------------------------

    #[test]
    fn create_known_issue_round_trips() {
        let conn = fresh_db();
        let id =
            create_known_issue(&conn, "Login button broken", Some("Users can't log in")).unwrap();
        assert!(id > 0);
        let issues = list_known_issues(&conn, None).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].name, "Login button broken");
        assert_eq!(issues[0].status, "active");
    }

    #[test]
    fn update_known_issue_status_works() {
        let conn = fresh_db();
        let id = create_known_issue(&conn, "Bug", None).unwrap();
        assert!(update_known_issue_status(&conn, id, "resolved").unwrap());
        let issues = list_known_issues(&conn, Some("resolved")).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn add_issue_link_works() {
        let conn = fresh_db();
        let ki_id = create_known_issue(&conn, "Bug", None).unwrap();
        let link_id = add_issue_link(&conn, ki_id, 1001, "related").unwrap();
        assert!(link_id > 0);
    }

    #[test]
    fn list_known_issues_filters_by_status() {
        let conn = fresh_db();
        create_known_issue(&conn, "Active bug", None).unwrap();
        create_known_issue(&conn, "Another", None).unwrap();
        update_known_issue_status(&conn, 1, "resolved").unwrap();

        let active = list_known_issues(&conn, Some("active")).unwrap();
        let resolved = list_known_issues(&conn, Some("resolved")).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(resolved.len(), 1);
    }

    // ---- M7-T03: Issue clusters --------------------------------------------

    #[test]
    fn create_issue_cluster_and_add_members() {
        let conn = fresh_db();
        let cluster_id = create_issue_cluster(&conn, "Login issues cluster").unwrap();
        add_cluster_member(&conn, cluster_id, 1001).unwrap();
        add_cluster_member(&conn, cluster_id, 1002).unwrap();
        add_cluster_member(&conn, cluster_id, 1003).unwrap();

        let clusters = list_issue_clusters(&conn).unwrap();
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].conversation_count, 3);
    }

    #[test]
    fn add_cluster_member_is_idempotent() {
        let conn = fresh_db();
        let cluster_id = create_issue_cluster(&conn, "Cluster").unwrap();
        add_cluster_member(&conn, cluster_id, 1001).unwrap();
        add_cluster_member(&conn, cluster_id, 1001).unwrap(); // duplicate
        let clusters = list_issue_clusters(&conn).unwrap();
        assert_eq!(
            clusters[0].conversation_count, 1,
            "duplicate member not counted"
        );
    }

    #[test]
    fn list_issue_clusters_empty() {
        let conn = fresh_db();
        assert!(list_issue_clusters(&conn).unwrap().is_empty());
    }

    // ---- M7-T04: Issue Radar -----------------------------------------------

    #[test]
    fn radar_snapshot_empty() {
        let conn = fresh_db();
        let snapshot = get_radar_snapshot(&conn).unwrap();
        assert_eq!(snapshot.active_known_issues, 0);
        assert_eq!(snapshot.active_clusters, 0);
        assert_eq!(snapshot.active_incidents, 0);
    }

    #[test]
    fn radar_snapshot_counts_active_items() {
        let conn = fresh_db();
        create_known_issue(&conn, "Bug 1", None).unwrap();
        create_known_issue(&conn, "Bug 2", None).unwrap();
        update_known_issue_status(&conn, 1, "resolved").unwrap(); // only 1 active
        create_issue_cluster(&conn, "Cluster 1").unwrap();
        promote_to_incident(
            &conn,
            None,
            IncidentSeverity::Sev2,
            IncidentSource::Manual,
            None,
        )
        .unwrap();

        let snapshot = get_radar_snapshot(&conn).unwrap();
        assert_eq!(snapshot.active_known_issues, 1);
        assert_eq!(snapshot.active_clusters, 1);
        assert_eq!(snapshot.active_incidents, 1);
    }

    // ---- M7-T05: Incidents -------------------------------------------------

    #[test]
    fn promote_to_incident_creates_with_correct_severity_and_source() {
        let conn = fresh_db();
        let ki_id = create_known_issue(&conn, "Bug", None).unwrap();
        let id = promote_to_incident(
            &conn,
            Some(ki_id),
            IncidentSeverity::Sev1,
            IncidentSource::KnownIssue,
            Some("Critical"),
        )
        .unwrap();
        assert!(id > 0);

        let incidents = list_incidents(&conn, None).unwrap();
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].severity, "sev1");
        assert_eq!(incidents[0].source, "known_issue");
        assert_eq!(incidents[0].status, "investigating");
    }

    #[test]
    fn update_incident_status_to_resolved() {
        let conn = fresh_db();
        let id = promote_to_incident(
            &conn,
            None,
            IncidentSeverity::Sev3,
            IncidentSource::Manual,
            None,
        )
        .unwrap();
        assert!(update_incident_status(&conn, id, IncidentStatus::Resolved).unwrap());

        let resolved = list_incidents(&conn, Some(IncidentStatus::Resolved)).unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].resolved_at.is_some());
    }

    #[test]
    fn list_incidents_filters_by_status() {
        let conn = fresh_db();
        promote_to_incident(
            &conn,
            None,
            IncidentSeverity::Sev3,
            IncidentSource::Manual,
            None,
        )
        .unwrap();
        promote_to_incident(
            &conn,
            None,
            IncidentSeverity::Sev2,
            IncidentSource::Cluster,
            None,
        )
        .unwrap();
        update_incident_status(&conn, 1, IncidentStatus::Identified).unwrap();

        let investigating = list_incidents(&conn, Some(IncidentStatus::Investigating)).unwrap();
        let identified = list_incidents(&conn, Some(IncidentStatus::Identified)).unwrap();
        assert_eq!(investigating.len(), 1);
        assert_eq!(identified.len(), 1);
    }

    // ---- M7-T06: SLA -------------------------------------------------------

    #[test]
    fn set_and_get_sla_config() {
        let conn = fresh_db();
        set_sla_config(&conn, 101, 4.0, 48.0).unwrap();
        let config = get_sla_config(&conn, 101).unwrap().unwrap();
        assert_eq!(config.mailbox_id, 101);
        assert!((config.first_response_hours - 4.0).abs() < 1e-6);
        assert!((config.resolution_hours - 48.0).abs() < 1e-6);
    }

    #[test]
    fn get_sla_config_returns_none_for_nonexistent() {
        let conn = fresh_db();
        assert!(get_sla_config(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn set_sla_config_upserts() {
        let conn = fresh_db();
        set_sla_config(&conn, 101, 4.0, 48.0).unwrap();
        set_sla_config(&conn, 101, 8.0, 72.0).unwrap(); // overwrite
        let config = get_sla_config(&conn, 101).unwrap().unwrap();
        assert!((config.first_response_hours - 8.0).abs() < 1e-6);
    }

    #[test]
    fn record_and_count_sla_breaches() {
        let conn = fresh_db();
        assert_eq!(count_sla_breaches(&conn).unwrap(), 0);
        record_sla_breach(&conn, 1001, "first_response").unwrap();
        record_sla_breach(&conn, 1002, "resolution").unwrap();
        assert_eq!(count_sla_breaches(&conn).unwrap(), 2);
    }

    // ---- M7-T07: Knowledge docs — freshness + gaps ------------------------

    #[test]
    fn record_knowledge_gap_increments_count() {
        let conn = fresh_db();
        record_knowledge_gap(&conn, "how to configure webhook").unwrap();
        record_knowledge_gap(&conn, "how to configure webhook").unwrap();
        record_knowledge_gap(&conn, "how to configure webhook").unwrap();
        record_knowledge_gap(&conn, "password reset").unwrap();

        let gaps = list_knowledge_gaps(&conn, 10).unwrap();
        assert_eq!(gaps.len(), 2);
        // The one with 3 hits should be first (sorted by hit_count DESC).
        assert_eq!(gaps[0].0, "how to configure webhook");
        assert_eq!(gaps[0].1, 3);
    }

    #[test]
    fn list_knowledge_gaps_empty() {
        let conn = fresh_db();
        assert!(list_knowledge_gaps(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn update_doc_freshness_and_count_stale() {
        let conn = fresh_db();
        assert_eq!(count_stale_docs(&conn).unwrap(), 0);
        update_doc_freshness(&conn, 1, Some("hash1"), false).unwrap();
        update_doc_freshness(&conn, 2, Some("hash2"), true).unwrap();
        update_doc_freshness(&conn, 3, Some("hash3"), true).unwrap();
        assert_eq!(count_stale_docs(&conn).unwrap(), 2);
    }

    // ---- serde --------------------------------------------------------------

    #[test]
    fn known_issue_serializes() {
        let ki = KnownIssue {
            id: Some(1),
            name: "Bug".into(),
            status: "active".into(),
            description: Some("desc".into()),
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: "2026-10-01T10:00:00Z".into(),
        };
        let s = serde_json::to_string(&ki).unwrap();
        assert!(s.contains("\"name\":\"Bug\""));
        assert!(s.contains("\"status\":\"active\""));
    }

    #[test]
    fn incident_serializes() {
        let inc = Incident {
            id: Some(1),
            known_issue_id: Some(42),
            status: "investigating".into(),
            severity: "sev1".into(),
            source: "manual".into(),
            description: None,
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: "2026-10-01T10:00:00Z".into(),
            resolved_at: None,
        };
        let s = serde_json::to_string(&inc).unwrap();
        assert!(s.contains("\"severity\":\"sev1\""));
    }

    #[test]
    fn radar_snapshot_serializes() {
        let rs = RadarSnapshot {
            active_known_issues: 3,
            active_clusters: 2,
            active_incidents: 1,
        };
        let s = serde_json::to_string(&rs).unwrap();
        assert!(s.contains("\"active_known_issues\":3"));
    }
}
