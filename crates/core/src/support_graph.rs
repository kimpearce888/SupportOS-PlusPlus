//! Human-asserted support-graph edges — the GR-02 port of the reference
//! `graphService.ts` human-edge half (v2.2.0, plan Phase 34).
//!
//! The support graph is a RELATIONSHIP LAYER over the mirror tables, not a
//! graph database. Derived edges are computed at read time (they land with
//! plan items GR-01/GR-03); only HUMAN judgments are persisted, because a
//! human judgment is information the database does not already contain.
//!
//! Reference contract (supportos @ c346fb51):
//! - `src/server/database/migrations/016_m6_graph_coaching_memory.ts:63-90`
//!   — the `support_graph_edges` DDL. Stack adaptation (documented rename,
//!   see db_breadth.rs): the port's table keeps the name `graph_edges`.
//! - `src/server/graph/graphService.ts:816-872` — linkHumanEdge (self /
//!   source-404 / target-404 / duplicate checks, in that order),
//!   unlinkHumanEdge, getHumanEdge, listHumanEdges.
//! - `src/shared/graph.ts:76-77` — the closed 5-relation union
//!   `GRAPH_HUMAN_RELATIONS` (`related_to`, `depends_on`, `blocks`,
//!   `mentions`, `duplicate_of`).
//!
//! Node endpoints are addressed by (kind, local mirror id) against the 12
//! closed node kinds, exactly like the reference `NODE_EXISTS_SQL` /
//! `NODE_LABEL_SQL` maps — the port resolves each kind against its own
//! mirror table, with the port's column names mapped at the query
//! boundary (DB-04).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{GraphHumanRelation, GraphNodeKind};
use crate::error::Result;

// ---------------------------------------------------------------------------
// Schema (reference migration 016, documented rename support_graph_edges→graph_edges)
// ---------------------------------------------------------------------------

/// The human-edge store. Every stored edge is a human judgment: the closed
/// 12-kind union on both endpoints, the closed 5-relation vocabulary, a
/// note, the creating user, and provenance (always `human_local` through
/// this API — the reference's only writer).
const GRAPH_EDGES_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS graph_edges (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        source_kind TEXT NOT NULL
          CHECK (source_kind IN (
            'customer','organization','conversation','known_issue','issue_cluster',
            'incident','knowledge_document','agent','campaign','product',
            'custom_object','connector_data'
          )),
        source_local_id INTEGER NOT NULL,
        target_kind TEXT NOT NULL
          CHECK (target_kind IN (
            'customer','organization','conversation','known_issue','issue_cluster',
            'incident','knowledge_document','agent','campaign','product',
            'custom_object','connector_data'
          )),
        target_local_id INTEGER NOT NULL,
        relation TEXT NOT NULL
          CHECK (relation IN ('related_to','depends_on','blocks','mentions','duplicate_of')),
        note TEXT,
        created_by_user_local_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        provenance TEXT NOT NULL DEFAULT 'human_local',
        UNIQUE (source_kind, source_local_id, target_kind, target_local_id, relation)
    );
    CREATE INDEX IF NOT EXISTS idx_graph_edges_source
        ON graph_edges (source_kind, source_local_id);
    CREATE INDEX IF NOT EXISTS idx_graph_edges_target
        ON graph_edges (target_kind, target_local_id);
"#;

/// Ensure the human-edge store exists in the reference shape.
///
/// Legacy databases (the pre-GR-02 `graph_edges` was a graph_nodes-surrogate
/// model: `source_id`/`target_id` → graph_nodes.id + free-text `edge_type`)
/// are reshaped by drop-and-recreate. That is safe because the legacy table's
/// ONLY writer was the old unvalidated `POST /api/graph/edges` — exactly the
/// route this module replaces: nothing in the boot chain, the sync engine or
/// the demo world ever wrote it, and its rows (arbitrary kinds, the
/// out-of-vocabulary `'related'` default) cannot be mapped into the
/// reference's (kind, local_id, relation) contract.
pub fn ensure_graph_edges_schema(conn: &Connection) -> Result<()> {
    let legacy_shape = table_columns(conn, "graph_edges")?
        .iter()
        .any(|c| c == "source_id");
    if legacy_shape {
        conn.execute("DROP TABLE graph_edges", [])?;
    }
    conn.execute_batch(GRAPH_EDGES_SQL)?;
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols)
}

// ---------------------------------------------------------------------------
// Node references (reference NODE_EXISTS_SQL / NODE_LABEL_SQL, ported)
// ---------------------------------------------------------------------------

/// A resolved graph node endpoint (reference `GraphNodeRef`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNodeRef {
    pub kind: String,
    pub local_id: i64,
    /// Human label (name, subject, code...) resolved in the same query.
    pub label: String,
    /// Secondary label (organization, mailbox, status...) when useful.
    pub sublabel: Option<String>,
    pub deleted: bool,
}

impl GraphNodeRef {
    /// The tombstone the reference serves when an edge outlives its endpoint
    /// (`graphService.ts` `humanRef`/`fallback`): honest about the removal
    /// instead of hiding the edge.
    fn removed(kind: GraphNodeKind, local_id: i64) -> Self {
        Self {
            kind: kind.as_str().to_string(),
            local_id,
            label: format!("#{local_id} (removed)"),
            sublabel: None,
            deleted: true,
        }
    }

    fn from_row(kind: GraphNodeKind, r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            kind: kind.as_str().to_string(),
            local_id: r.get(0)?,
            label: r.get(1)?,
            sublabel: r.get(2)?,
            deleted: r.get::<_, i64>(3)? != 0,
        })
    }
}

/// Does a mirror row exist for (kind, local id)? (reference `nodeExists`,
/// the 404 gate for both endpoints of every human edge.)
pub fn node_exists(conn: &Connection, kind: GraphNodeKind, id: i64) -> bool {
    let table = match kind {
        GraphNodeKind::Customer => "customers",
        GraphNodeKind::Organization => "organizations",
        GraphNodeKind::Conversation => "conversations",
        GraphNodeKind::KnownIssue => "known_issues",
        GraphNodeKind::IssueCluster => "issue_clusters",
        GraphNodeKind::Incident => "incidents",
        GraphNodeKind::KnowledgeDocument => "knowledge_documents",
        GraphNodeKind::Agent => "users",
        GraphNodeKind::Campaign => "outreach_campaigns",
        GraphNodeKind::Product => "products",
        GraphNodeKind::CustomObject => "custom_objects",
        GraphNodeKind::ConnectorData => "connector_rows",
    };
    conn.query_row(
        &format!("SELECT 1 FROM {table} WHERE id = ?1"),
        params![id],
        |_| Ok(()),
    )
    .is_ok()
}

/// Resolve a node's label/sublabel/deleted marker (reference `NODE_LABEL_SQL`,
/// ported to the port's mirror shapes — labels resolve in the SAME single
/// query per kind, never N+1):
///
/// Port column adaptations (documented, DB-04):
/// - customer: `customers.organization_id` is a guarded column; the legacy
///   `customers.organization` name string stays the sublabel fallback.
/// - known_issue / issue_cluster: the base tables carry `name` (M015/M016);
///   the reference-shaped `title` is a guarded column — `title` wins.
/// - incident: `code`/`title` are guarded columns legacy rows may lack; the
///   reference's `code || ' ' || title` gets an 'Incident #id' fallback.
/// - kinds whose mirror tables have no soft-delete column report `false`.
pub fn node_ref(conn: &Connection, kind: GraphNodeKind, id: i64) -> Result<Option<GraphNodeRef>> {
    let sql = match kind {
        GraphNodeKind::Customer => "SELECT cu.id,
                COALESCE(NULLIF(TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')), ''), 'Customer #' || cu.id),
                COALESCE(o.name, NULLIF(TRIM(cu.organization), '')),
                (cu.deleted_at IS NOT NULL)
             FROM customers cu LEFT JOIN organizations o ON o.id = cu.organization_id
             WHERE cu.id = ?1",
        GraphNodeKind::Organization => "SELECT o.id, o.name, o.domains, (o.deleted_at IS NOT NULL)
             FROM organizations o WHERE o.id = ?1",
        GraphNodeKind::Conversation => {
            "SELECT c.id, '#' || c.number || ' ' || COALESCE(SUBSTR(c.subject, 1, 100), '(no subject)'),
                c.status, (c.deleted_at IS NOT NULL)
             FROM conversations c WHERE c.id = ?1"
        }
        GraphNodeKind::KnownIssue => {
            "SELECT ki.id, COALESCE(NULLIF(TRIM(ki.title), ''), ki.name), ki.status, 0
             FROM known_issues ki WHERE ki.id = ?1"
        }
        GraphNodeKind::IssueCluster => {
            "SELECT ic.id, COALESCE(NULLIF(TRIM(ic.title), ''), ic.name), 'cluster', 0
             FROM issue_clusters ic WHERE ic.id = ?1"
        }
        GraphNodeKind::Incident => {
            "SELECT i.id, COALESCE(NULLIF(TRIM(COALESCE(i.code, '') || ' ' || COALESCE(i.title, '')), ''), 'Incident #' || i.id),
                i.status, 0
             FROM incidents i WHERE i.id = ?1"
        }
        GraphNodeKind::KnowledgeDocument => {
            "SELECT kd.id, kd.title, kd.visibility, 0 FROM knowledge_documents kd WHERE kd.id = ?1"
        }
        GraphNodeKind::Agent => {
            "SELECT u.id,
                COALESCE(NULLIF(TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')), ''), 'User #' || u.id),
                u.email, (u.deleted_at IS NOT NULL)
             FROM users u WHERE u.id = ?1"
        }
        GraphNodeKind::Campaign => {
            "SELECT oc.id, oc.name, oc.status, 0 FROM outreach_campaigns oc WHERE oc.id = ?1"
        }
        GraphNodeKind::Product => {
            "SELECT p.id, p.name, p.description, 0 FROM products p WHERE p.id = ?1"
        }
        GraphNodeKind::CustomObject => {
            "SELECT co.id, co.title, cot.name, (co.deleted_at IS NOT NULL)
             FROM custom_objects co JOIN custom_object_types cot ON cot.id = co.type_id
             WHERE co.id = ?1"
        }
        GraphNodeKind::ConnectorData => {
            "SELECT cr.id, 'row ' || cr.row_key, cn.name, 0
             FROM connector_rows cr JOIN connectors cn ON cn.id = cr.connector_id
             WHERE cr.id = ?1"
        }
    };
    let row = conn.query_row(sql, params![id], |r| GraphNodeRef::from_row(kind, r));
    Ok(row.ok())
}

/// Resolve a node ref, serving the reference tombstone when the mirror row
/// is gone (an edge that outlives its endpoint is still shown, honestly).
fn node_ref_or_tombstone(conn: &Connection, kind: GraphNodeKind, id: i64) -> Result<GraphNodeRef> {
    Ok(node_ref(conn, kind, id)?.unwrap_or_else(|| GraphNodeRef::removed(kind, id)))
}

// ---------------------------------------------------------------------------
// Link / unlink / read (reference graphService.ts:816-872)
// ---------------------------------------------------------------------------

/// The wire payload of a human edge (reference `GraphHumanEdge`).
#[derive(Debug, Clone, PartialEq)]
pub struct HumanEdge {
    pub id: i64,
    pub source: GraphNodeRef,
    pub target: GraphNodeRef,
    pub relation: GraphHumanRelation,
    pub note: Option<String>,
    pub created_at: String,
    pub created_by: Option<String>,
}

impl HumanEdge {
    pub fn to_json(&self) -> Value {
        let source = serde_json::to_value(&self.source).unwrap_or(Value::Null);
        let target = serde_json::to_value(&self.target).unwrap_or(Value::Null);
        json!({
            "id": self.id,
            "source": source,
            "target": target,
            "relation": self.relation.as_str(),
            "note": self.note,
            "created_at": self.created_at,
            "created_by": self.created_by,
        })
    }
}

/// The ordered failure vocabulary of `linkHumanEdge` — the route maps each
/// code to the reference status + message (graph.ts:156-167).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// Same (kind, id) on both ends → 409 Conflict.
    SelfEdge,
    /// Source mirror row missing → 404 NotFound.
    SourceNotFound,
    /// Target mirror row missing → 404 NotFound.
    TargetNotFound,
    /// The exact 5-tuple already exists → 409 Conflict.
    Duplicate,
}

/// A validated link request (the POST body after Zod-parity validation).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkInput {
    pub source_kind: GraphNodeKind,
    pub source_local_id: i64,
    pub target_kind: GraphNodeKind,
    pub target_local_id: i64,
    pub relation: GraphHumanRelation,
    pub note: Option<String>,
    /// The route always links as the anonymous local user (the reference
    /// passes `user_local_id: null`); the column exists for parity.
    pub user_local_id: Option<i64>,
}

/// Assert a human edge. Check order matches the reference exactly:
/// self-edge first, then source existence, then target existence, then the
/// duplicate probe — so the surfaced failure is deterministic.
pub fn link_human_edge(
    conn: &Connection,
    input: &LinkInput,
) -> std::result::Result<HumanEdge, LinkError> {
    if input.source_kind == input.target_kind && input.source_local_id == input.target_local_id {
        return Err(LinkError::SelfEdge);
    }
    if !node_exists(conn, input.source_kind, input.source_local_id) {
        return Err(LinkError::SourceNotFound);
    }
    if !node_exists(conn, input.target_kind, input.target_local_id) {
        return Err(LinkError::TargetNotFound);
    }
    let duplicate: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM graph_edges
              WHERE source_kind = ?1 AND source_local_id = ?2
                AND target_kind = ?3 AND target_local_id = ?4 AND relation = ?5",
            params![
                input.source_kind.as_str(),
                input.source_local_id,
                input.target_kind.as_str(),
                input.target_local_id,
                input.relation.as_str()
            ],
            |r| r.get(0),
        )
        .ok();
    if duplicate.is_some() {
        return Err(LinkError::Duplicate);
    }
    conn.execute(
        "INSERT INTO graph_edges
             (source_kind, source_local_id, target_kind, target_local_id,
              relation, note, created_by_user_local_id, created_at, provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), 'human_local')",
        params![
            input.source_kind.as_str(),
            input.source_local_id,
            input.target_kind.as_str(),
            input.target_local_id,
            input.relation.as_str(),
            input.note,
            input.user_local_id,
        ],
    )
    .map_err(|_| LinkError::Duplicate)?;
    get_human_edge(conn, conn.last_insert_rowid()).ok_or(LinkError::Duplicate)
}

/// Remove a human edge; `false` when the id is unknown (the route's 404).
pub fn unlink_human_edge(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM graph_edges WHERE id = ?1", params![id])? > 0)
}

/// Serve one human edge with both endpoints resolved (reference
/// `getHumanEdge`): created_by resolves to the creating user's name with the
/// reference's `'User #' || id` fallback, and stays null for the anonymous
/// links this API writes.
pub fn get_human_edge(conn: &Connection, id: i64) -> Option<HumanEdge> {
    struct Row {
        source_kind: String,
        source_local_id: i64,
        target_kind: String,
        target_local_id: i64,
        relation: String,
        note: Option<String>,
        created_at: String,
        created_by: Option<String>,
    }
    let row = conn
        .query_row(
            "SELECT e.source_kind, e.source_local_id, e.target_kind, e.target_local_id,
                    e.relation, e.note, e.created_at,
                    COALESCE(NULLIF(TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')), ''),
                             'User #' || u.id)
                FROM graph_edges e LEFT JOIN users u ON u.id = e.created_by_user_local_id
               WHERE e.id = ?1",
            params![id],
            |r| {
                Ok(Row {
                    source_kind: r.get(0)?,
                    source_local_id: r.get(1)?,
                    target_kind: r.get(2)?,
                    target_local_id: r.get(3)?,
                    relation: r.get(4)?,
                    note: r.get(5)?,
                    created_at: r.get(6)?,
                    created_by: r.get(7)?,
                })
            },
        )
        .ok()?;
    let source_kind = GraphNodeKind::ALL
        .into_iter()
        .find(|k| k.as_str() == row.source_kind)?;
    let target_kind = GraphNodeKind::ALL
        .into_iter()
        .find(|k| k.as_str() == row.target_kind)?;
    let relation = GraphHumanRelation::parse(&row.relation)?;
    let source = node_ref_or_tombstone(conn, source_kind, row.source_local_id).ok()?;
    let target = node_ref_or_tombstone(conn, target_kind, row.target_local_id).ok()?;
    Some(HumanEdge {
        id,
        source,
        target,
        relation,
        note: row.note,
        created_at: row.created_at,
        created_by: row.created_by,
    })
}

/// List human edges, newest first (reference `listHumanEdges`: ORDER BY
/// created_at DESC, id DESC; limit clamped by the route, offset >= 0).
pub fn list_human_edges(
    conn: &Connection,
    limit: i64,
    offset: i64,
) -> Result<(Vec<HumanEdge>, i64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT id FROM graph_edges ORDER BY created_at DESC, id DESC LIMIT ?1 OFFSET ?2",
    )?;
    let ids = stmt
        .query_map(params![limit, offset], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    let edges = ids
        .iter()
        .filter_map(|id| get_human_edge(conn, *id))
        .collect();
    Ok((edges, total))
}

/// All human edges touching (kind, local id) in BOTH directions, each as the
/// reference `GraphEdge` wire shape ({relation, origin, source, target,
/// note, at}) with BOTH endpoints resolved. The interim neighbors/subgraph
/// routes and the AI graph tools read this until the derived edge layer
/// lands (GR-01/GR-03).
pub fn human_edges_touching(
    conn: &Connection,
    kind: GraphNodeKind,
    local_id: i64,
) -> Result<Vec<Value>> {
    /// (is_outgoing, relation, note, at, other_kind, other_id)
    type TouchingRow = (bool, String, Option<String>, Option<String>, String, i64);

    let center = node_ref_or_tombstone(conn, kind, local_id)?;
    let mut raw: Vec<TouchingRow> = Vec::new();

    {
        let mut stmt = conn.prepare(
            "SELECT relation, note, created_at, target_kind, target_local_id
               FROM graph_edges WHERE source_kind = ?1 AND source_local_id = ?2
               ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![kind.as_str(), local_id], |r| {
                Ok((true, r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.extend(rows);
    }
    {
        let mut stmt = conn.prepare(
            "SELECT relation, note, created_at, source_kind, source_local_id
               FROM graph_edges WHERE target_kind = ?1 AND target_local_id = ?2
               ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![kind.as_str(), local_id], |r| {
                Ok((false, r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.extend(rows);
    }

    let mut out = Vec::with_capacity(raw.len());
    for (outgoing, relation, note, at, other_kind_str, other_id) in raw {
        let Some(other_kind) = GraphNodeKind::ALL
            .into_iter()
            .find(|k| k.as_str() == other_kind_str)
        else {
            continue;
        };
        let other = node_ref_or_tombstone(conn, other_kind, other_id)?;
        let (source, target) = if outgoing {
            (center.clone(), other)
        } else {
            (other, center.clone())
        };
        out.push(json!({
            "relation": relation,
            "origin": "human_local",
            "source": source,
            "target": target,
            "note": note,
            "at": at,
        }));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// POST body validation — Zod parity (zod 3.24.2, verified message-for-message)
// ---------------------------------------------------------------------------

/// `'customer' | 'organization' | ...` — the expected-options fragment zod
/// renders for enum issues.
fn kind_options() -> String {
    GraphNodeKind::ALL
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn relation_options() -> String {
    GraphHumanRelation::ALL
        .iter()
        .map(|r| format!("'{}'", r.as_str()))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// zod's `parsedType` rendering of a received JSON value ("number" here means
/// integer-valued; non-integers surface as "float" in int-check messages).
fn received_type(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(n) if n.is_f64() && n.as_f64().map(|f| f.fract() != 0.0).unwrap_or(false) => {
            "float"
        }
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

/// Validate one closed-union enum field the way z.enum does in 3.24:
/// missing → "Required"; a wrong STRING → "Invalid enum value. Expected ...,
/// received 'x'"; any non-string → "Expected ..., received <type>".
fn validate_enum_field(
    body: &Value,
    field: &str,
    options: &str,
    variants: &[&str],
) -> std::result::Result<String, String> {
    match body.get(field) {
        None => Err("Required".to_string()),
        Some(Value::String(s)) => {
            if variants.contains(&s.as_str()) {
                Ok(s.clone())
            } else {
                Err(format!(
                    "Invalid enum value. Expected {options}, received '{s}'"
                ))
            }
        }
        Some(v) => Err(format!("Expected {options}, received {}", received_type(v))),
    }
}

/// Validate `z.number().int().min(1)`.
fn validate_pos_int(body: &Value, field: &str) -> std::result::Result<i64, String> {
    match body.get(field) {
        None => Err("Required".to_string()),
        Some(v) if v.is_number() => {
            let f = v.as_f64().unwrap_or(f64::NAN);
            if f.fract() != 0.0 {
                Err("Expected integer, received float".to_string())
            } else if f < 1.0 {
                Err("Number must be greater than or equal to 1".to_string())
            } else {
                Ok(f as i64)
            }
        }
        Some(v) => Err(format!("Expected number, received {}", received_type(v))),
    }
}

/// The full POST /api/graph/edges schema:
/// `{source_kind: z.enum(12), source_local_id: z.number().int().min(1),
///   target_kind: z.enum(12), target_local_id: z.number().int().min(1),
///   relation: z.enum(5), note: z.string().max(500).nullable().optional()}`
/// (reference graph.ts:136-146). All issues are collected in schema order
/// like zod; the route surfaces the first and lists up to 10.
pub fn validate_link_body(
    body: &Value,
) -> std::result::Result<LinkInput, Vec<(&'static str, String)>> {
    let mut issues: Vec<(&'static str, String)> = Vec::new();
    if !body.is_object() {
        return Err(vec![(
            "",
            format!("Expected object, received {}", received_type(body)),
        )]);
    }

    let kind_variants: Vec<&str> = GraphNodeKind::ALL.iter().map(|k| k.as_str()).collect();
    let relation_variants: Vec<&str> = GraphHumanRelation::ALL.iter().map(|r| r.as_str()).collect();
    let kinds = kind_options();
    let relations = relation_options();

    let source_kind =
        validate_enum_field(body, "source_kind", &kinds, &kind_variants).and_then(|s| {
            GraphNodeKind::ALL
                .into_iter()
                .find(|k| k.as_str() == s)
                .ok_or_else(|| "Required".to_string())
        });
    let source_local_id = validate_pos_int(body, "source_local_id");
    let target_kind =
        validate_enum_field(body, "target_kind", &kinds, &kind_variants).and_then(|s| {
            GraphNodeKind::ALL
                .into_iter()
                .find(|k| k.as_str() == s)
                .ok_or_else(|| "Required".to_string())
        });
    let target_local_id = validate_pos_int(body, "target_local_id");
    let relation = validate_enum_field(body, "relation", &relations, &relation_variants)
        .and_then(|s| GraphHumanRelation::parse(&s).ok_or_else(|| "Required".to_string()));
    let note: std::result::Result<Option<String>, String> = match body.get("note") {
        None => Ok(None),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= 500 => Ok(Some(s.clone())),
        Some(Value::String(_)) => Err("String must contain at most 500 character(s)".to_string()),
        Some(v) => Err(format!("Expected string, received {}", received_type(v))),
    };

    if let Err(m) = &source_kind {
        issues.push(("source_kind", m.clone()));
    }
    if let Err(m) = &source_local_id {
        issues.push(("source_local_id", m.clone()));
    }
    if let Err(m) = &target_kind {
        issues.push(("target_kind", m.clone()));
    }
    if let Err(m) = &target_local_id {
        issues.push(("target_local_id", m.clone()));
    }
    if let Err(m) = &relation {
        issues.push(("relation", m.clone()));
    }
    if let Err(m) = &note {
        issues.push(("note", m.clone()));
    }

    let (source_kind, source_local_id, target_kind, target_local_id, relation, note) = match (
        source_kind,
        source_local_id,
        target_kind,
        target_local_id,
        relation,
        note,
    ) {
        (
            Ok(source_kind),
            Ok(source_local_id),
            Ok(target_kind),
            Ok(target_local_id),
            Ok(relation),
            Ok(note),
        ) => (
            source_kind,
            source_local_id,
            target_kind,
            target_local_id,
            relation,
            note,
        ),
        _ => return Err(issues),
    };

    Ok(LinkInput {
        source_kind,
        source_local_id,
        target_kind,
        target_local_id,
        relation,
        note,
        user_local_id: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::bootstrap::apply_all(&mut conn).expect("boot chain");
        conn
    }

    #[test]
    fn ensure_schema_is_the_reference_shape_and_idempotent() {
        let conn = db();
        for _ in 0..2 {
            ensure_graph_edges_schema(&conn).unwrap();
        }
        let cols = table_columns(&conn, "graph_edges").unwrap();
        for col in [
            "id",
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
            assert!(cols.iter().any(|c| c == col), "missing graph_edges.{col}");
        }
        // The CHECK constraints hold the closed vocabularies: an out-of-
        // vocabulary relation is rejected by the database itself.
        assert!(conn
            .execute(
                "INSERT INTO graph_edges (source_kind, source_local_id, target_kind, target_local_id, relation)
                 VALUES ('customer', 1, 'customer', 2, 'related')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO graph_edges (source_kind, source_local_id, target_kind, target_local_id, relation)
                 VALUES ('robot', 1, 'customer', 2, 'related_to')",
                [],
            )
            .is_err());
    }

    #[test]
    fn legacy_shape_is_reshaped() {
        let conn = db();
        // A pre-GR-02 database: the graph_nodes-surrogate edge model.
        conn.execute_batch(
            "DROP TABLE graph_edges;
             CREATE TABLE graph_edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id INTEGER NOT NULL,
                target_id INTEGER NOT NULL,
                edge_type TEXT NOT NULL DEFAULT 'related',
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             );
             INSERT INTO graph_edges (source_id, target_id, edge_type) VALUES (1, 2, 'filed');",
        )
        .unwrap();
        ensure_graph_edges_schema(&conn).unwrap();
        let cols = table_columns(&conn, "graph_edges").unwrap();
        assert!(
            !cols.iter().any(|c| c == "source_id"),
            "legacy shape survived"
        );
        assert!(cols.iter().any(|c| c == "relation"));
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "legacy garbage rows cannot survive the reshape");
    }

    #[test]
    fn node_ref_resolves_every_kind_with_the_reference_labels() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO organizations (id, remote_id, name, domains) VALUES (5, 55, 'Acme', 'acme.io');
             INSERT INTO customers (id, remote_id, first_name, last_name, organization) VALUES (9, 99, 'Ada', 'Lovelace', 'Acme');
             UPDATE customers SET organization_id = 5 WHERE id = 9;
             INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
             INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id)
                 VALUES (3, 33, 33, 'Refund please', 'active', 1, 9);
             INSERT INTO known_issues (id, name, status) VALUES (7, 'Login bug', 'resolved');
             UPDATE known_issues SET title = 'Login bug (titled)' WHERE id = 7;
             INSERT INTO issue_clusters (id, name) VALUES (4, 'Billing');
             INSERT INTO incidents (id, code, title, status, severity, source) VALUES (2, 'INC-1', 'Outage', 'resolved', 'sev2', 'manual');
             INSERT INTO knowledge_sources (id, name) VALUES (1, 'Runbooks');
             INSERT INTO knowledge_documents (id, source_id, title, visibility) VALUES (6, 1, 'How to reset', 'public');
             INSERT INTO users (id, remote_id, first_name, last_name, email) VALUES (8, 88, 'Grace', 'Hopper', 'grace@example.com');
             INSERT INTO outreach_campaigns (id, name, subject, body, status) VALUES (10, 'Winback', 'Hi', 'Hello', 'draft');
             INSERT INTO products (id, name, description) VALUES (11, 'Widget', 'The widget');
             INSERT INTO custom_object_types (id, name, slug) VALUES (1, 'VIP', 'vip');
             INSERT INTO custom_objects (id, type_id, title) VALUES (12, 1, 'Gold tier');
             INSERT INTO connectors (id, name, kind) VALUES (1, 'Stripe', 'stripe');
             INSERT INTO connector_rows (id, connector_id, row_key, data, fetched_at) VALUES (13, 1, 'ch_1', '{}', datetime('now'));",
        )
        .unwrap();

        let r = node_ref(&conn, GraphNodeKind::Customer, 9)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Ada Lovelace");
        assert_eq!(r.sublabel.as_deref(), Some("Acme"));
        assert!(!r.deleted);

        let r = node_ref(&conn, GraphNodeKind::Organization, 5)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Acme");
        assert_eq!(r.sublabel.as_deref(), Some("acme.io"));

        let r = node_ref(&conn, GraphNodeKind::Conversation, 3)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "#33 Refund please");
        assert_eq!(r.sublabel.as_deref(), Some("active"));

        let r = node_ref(&conn, GraphNodeKind::KnownIssue, 7)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Login bug (titled)");
        assert_eq!(r.sublabel.as_deref(), Some("resolved"));

        let r = node_ref(&conn, GraphNodeKind::IssueCluster, 4)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Billing");
        assert_eq!(r.sublabel.as_deref(), Some("cluster"));

        let r = node_ref(&conn, GraphNodeKind::Incident, 2)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "INC-1 Outage");

        let r = node_ref(&conn, GraphNodeKind::KnowledgeDocument, 6)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "How to reset");
        assert_eq!(r.sublabel.as_deref(), Some("public"));

        let r = node_ref(&conn, GraphNodeKind::Agent, 8).unwrap().unwrap();
        assert_eq!(r.label, "Grace Hopper");
        assert_eq!(r.sublabel.as_deref(), Some("grace@example.com"));

        let r = node_ref(&conn, GraphNodeKind::Campaign, 10)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Winback");
        assert_eq!(r.sublabel.as_deref(), Some("draft"));

        let r = node_ref(&conn, GraphNodeKind::Product, 11)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Widget");

        let r = node_ref(&conn, GraphNodeKind::CustomObject, 12)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Gold tier");
        assert_eq!(r.sublabel.as_deref(), Some("VIP"));

        let r = node_ref(&conn, GraphNodeKind::ConnectorData, 13)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "row ch_1");
        assert_eq!(r.sublabel.as_deref(), Some("Stripe"));

        // Missing rows resolve to None (the routes' 404 gate).
        assert!(node_ref(&conn, GraphNodeKind::Customer, 404)
            .unwrap()
            .is_none());
        // Legacy incidents without code/title get the honest fallback.
        conn.execute(
            "INSERT INTO incidents (id, status, severity, source) VALUES (14, 'investigating', 'sev3', 'manual')",
            [],
        )
        .unwrap();
        let r = node_ref(&conn, GraphNodeKind::Incident, 14)
            .unwrap()
            .unwrap();
        assert_eq!(r.label, "Incident #14");
    }

    fn sample_input() -> LinkInput {
        LinkInput {
            source_kind: GraphNodeKind::Customer,
            source_local_id: 9,
            target_kind: GraphNodeKind::Conversation,
            target_local_id: 3,
            relation: GraphHumanRelation::RelatedTo,
            note: Some("escalation context".to_string()),
            user_local_id: None,
        }
    }

    fn seed_two_nodes(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO customers (id, remote_id, first_name, last_name) VALUES (9, 99, 'Ada', 'Lovelace');
             INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 11, 'Support');
             INSERT INTO conversations (id, remote_id, number, subject, status, mailbox_id, customer_id)
                 VALUES (3, 33, 33, 'Refund please', 'active', 1, 9);",
        )
        .unwrap();
    }

    #[test]
    fn link_unlink_round_trip_with_all_checks() {
        let conn = db();
        seed_two_nodes(&conn);

        // Unknown endpoints 404 — source first, then target (reference order).
        let mut missing = sample_input();
        missing.source_local_id = 999;
        assert_eq!(
            link_human_edge(&conn, &missing),
            Err(LinkError::SourceNotFound)
        );
        let mut missing = sample_input();
        missing.target_local_id = 999;
        assert_eq!(
            link_human_edge(&conn, &missing),
            Err(LinkError::TargetNotFound)
        );

        // Self edge 409s even when both endpoints exist.
        let mut selfy = sample_input();
        selfy.target_kind = GraphNodeKind::Customer;
        selfy.target_local_id = 9;
        assert_eq!(link_human_edge(&conn, &selfy), Err(LinkError::SelfEdge));

        // A good link round-trips with resolved refs.
        let edge = link_human_edge(&conn, &sample_input()).unwrap();
        assert_eq!(edge.relation, GraphHumanRelation::RelatedTo);
        assert_eq!(edge.source.label, "Ada Lovelace");
        assert_eq!(edge.target.label, "#33 Refund please");
        assert_eq!(edge.note.as_deref(), Some("escalation context"));
        assert_eq!(edge.created_by, None);
        assert!(!edge.created_at.is_empty());

        // The exact 5-tuple is a duplicate; a different relation is not.
        assert_eq!(
            link_human_edge(&conn, &sample_input()),
            Err(LinkError::Duplicate)
        );
        let mut other = sample_input();
        other.relation = GraphHumanRelation::DependsOn;
        assert!(link_human_edge(&conn, &other).is_ok());

        // Unlink: true once, false after (the route's 404).
        assert!(unlink_human_edge(&conn, edge.id).unwrap());
        assert!(!unlink_human_edge(&conn, edge.id).unwrap());
        let (_, total) = list_human_edges(&conn, 50, 0).unwrap();
        assert_eq!(total, 1, "only the depends_on edge remains");
    }

    #[test]
    fn list_orders_newest_first_and_paginates() {
        let conn = db();
        seed_two_nodes(&conn);
        for relation in [
            GraphHumanRelation::RelatedTo,
            GraphHumanRelation::Blocks,
            GraphHumanRelation::Mentions,
        ] {
            let mut input = sample_input();
            input.relation = relation;
            input.note = None;
            link_human_edge(&conn, &input).unwrap();
        }
        let (edges, total) = list_human_edges(&conn, 2, 0).unwrap();
        assert_eq!(total, 3);
        assert_eq!(edges.len(), 2);
        assert_eq!(
            edges[0].relation,
            GraphHumanRelation::Mentions,
            "newest first"
        );
        let (rest, total) = list_human_edges(&conn, 2, 2).unwrap();
        assert_eq!(total, 3);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].relation, GraphHumanRelation::RelatedTo);
    }

    #[test]
    fn edges_touching_serve_both_directions_and_tombstones() {
        let conn = db();
        seed_two_nodes(&conn);
        let edge = link_human_edge(&conn, &sample_input()).unwrap();

        let out = human_edges_touching(&conn, GraphNodeKind::Customer, 9).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["relation"], "related_to");
        assert_eq!(out[0]["origin"], "human_local");
        assert_eq!(out[0]["source"]["label"], "Ada Lovelace");
        assert_eq!(out[0]["target"]["label"], "#33 Refund please");

        let inc = human_edges_touching(&conn, GraphNodeKind::Conversation, 3).unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0]["relation"], "related_to");

        // An edge that outlives its endpoint is served with the tombstone.
        conn.execute("DELETE FROM conversations WHERE id = 3", [])
            .unwrap();
        let gone = human_edges_touching(&conn, GraphNodeKind::Customer, 9).unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0]["target"]["label"], "#3 (removed)");
        assert_eq!(gone[0]["target"]["deleted"], true);
        // get_humanEdge serves the same tombstone.
        let served = get_human_edge(&conn, edge.id).unwrap();
        assert_eq!(served.target.label, "#3 (removed)");
    }

    // ---- POST body validation: zod 3.24.2 parity ---------------------------

    const KINDS: &str = "'customer' | 'organization' | 'conversation' | 'known_issue' | 'issue_cluster' | 'incident' | 'knowledge_document' | 'agent' | 'campaign' | 'product' | 'custom_object' | 'connector_data'";
    const RELATIONS: &str = "'related_to' | 'depends_on' | 'blocks' | 'mentions' | 'duplicate_of'";

    fn issues_of(body: Value) -> Vec<(String, String)> {
        validate_link_body(&body)
            .map(|_| Vec::new())
            .unwrap_or_else(|issues| {
                issues
                    .iter()
                    .map(|(p, m)| (p.to_string(), m.clone()))
                    .collect()
            })
    }

    #[test]
    fn valid_body_parses_and_strips_unknown_keys() {
        let input = validate_link_body(&json!({
            "source_kind": "customer", "source_local_id": 9,
            "target_kind": "conversation", "target_local_id": 3,
            "relation": "duplicate_of", "note": null, "evil": "stripped"
        }))
        .unwrap();
        assert_eq!(input.source_kind, GraphNodeKind::Customer);
        assert_eq!(input.relation, GraphHumanRelation::DuplicateOf);
        assert_eq!(input.note, None);
    }

    #[test]
    fn empty_body_lists_every_required_field_in_schema_order() {
        let issues = issues_of(json!({}));
        assert_eq!(
            issues,
            vec![
                ("source_kind".to_string(), "Required".to_string()),
                ("source_local_id".to_string(), "Required".to_string()),
                ("target_kind".to_string(), "Required".to_string()),
                ("target_local_id".to_string(), "Required".to_string()),
                ("relation".to_string(), "Required".to_string()),
            ]
        );
    }

    #[test]
    fn wrong_enum_string_gets_the_zod_invalid_enum_value_message() {
        let issues = issues_of(json!({
            "source_kind": "robot", "source_local_id": 1,
            "target_kind": "customer", "target_local_id": 2,
            "relation": "related"
        }));
        assert_eq!(issues[0].0, "source_kind");
        assert_eq!(
            issues[0].1,
            format!("Invalid enum value. Expected {KINDS}, received 'robot'")
        );
        assert_eq!(issues[1].0, "relation");
        assert_eq!(
            issues[1].1,
            format!("Invalid enum value. Expected {RELATIONS}, received 'related'")
        );
    }

    #[test]
    fn non_string_enum_gets_the_zod_invalid_type_message() {
        let issues = issues_of(json!({
            "source_kind": 5, "source_local_id": 1,
            "target_kind": "customer", "target_local_id": 2,
            "relation": "related_to"
        }));
        assert_eq!(issues[0].1, format!("Expected {KINDS}, received number"));
    }

    #[test]
    fn number_field_messages_match_zod_exactly() {
        let base = json!({
            "source_kind": "customer",
            "target_kind": "customer",
            "target_local_id": 2,
            "relation": "related_to"
        });
        let mut b = base.clone();
        b["source_local_id"] = json!("3");
        assert_eq!(
            issues_of(b)[0],
            (
                "source_local_id".to_string(),
                "Expected number, received string".to_string()
            )
        );
        let mut b = base.clone();
        b["source_local_id"] = json!(1.5);
        assert_eq!(
            issues_of(b)[0],
            (
                "source_local_id".to_string(),
                "Expected integer, received float".to_string()
            )
        );
        let mut b = base.clone();
        b["source_local_id"] = json!(0);
        assert_eq!(
            issues_of(b)[0],
            (
                "source_local_id".to_string(),
                "Number must be greater than or equal to 1".to_string()
            )
        );
        let mut b = base.clone();
        b["source_local_id"] = json!(-7);
        assert_eq!(
            issues_of(b)[0],
            (
                "source_local_id".to_string(),
                "Number must be greater than or equal to 1".to_string()
            )
        );
        let mut b = base.clone();
        b["source_local_id"] = json!(true);
        assert_eq!(
            issues_of(b)[0],
            (
                "source_local_id".to_string(),
                "Expected number, received boolean".to_string()
            )
        );
        let mut b = base.clone();
        b["source_local_id"] = json!(null);
        assert_eq!(
            issues_of(b)[0],
            (
                "source_local_id".to_string(),
                "Expected number, received null".to_string()
            )
        );
    }

    #[test]
    fn note_messages_match_zod_exactly() {
        let base = json!({
            "source_kind": "customer", "source_local_id": 1,
            "target_kind": "customer", "target_local_id": 2,
            "relation": "related_to"
        });
        let mut b = base.clone();
        b["note"] = json!("x".repeat(501));
        assert_eq!(
            issues_of(b)[0],
            (
                "note".to_string(),
                "String must contain at most 500 character(s)".to_string()
            )
        );
        let mut b = base.clone();
        b["note"] = json!(42);
        assert_eq!(
            issues_of(b)[0],
            (
                "note".to_string(),
                "Expected string, received number".to_string()
            )
        );
        // A 500-char note is exactly at the cap.
        let mut b = base.clone();
        b["note"] = json!("x".repeat(500));
        assert!(validate_link_body(&b).is_ok());
    }

    #[test]
    fn non_object_body_is_a_root_level_issue() {
        let issues = issues_of(json!([1, 2]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].0, "");
        assert_eq!(issues[0].1, "Expected object, received array");
    }
}
