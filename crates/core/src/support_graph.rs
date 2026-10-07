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
/// note, at}) with BOTH endpoints resolved. The AI graph tools read this
/// directly; the neighbors/subgraph routes layer the derived edge layer
/// (GR-01) on top of it.
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

// ─── GR-01: the read-time derived edge layer ──────────────────────────────

/// One derived edge with resolved local-id endpoints.
struct DerivedEdge {
    relation: &'static str,
    origin: &'static str,
    source_kind: GraphNodeKind,
    source_id: i64,
    target_kind: GraphNodeKind,
    target_id: i64,
    note: Option<String>,
    at: Option<String>,
}

/// The ~24 read-time derived branches. Every edge is derived LIVE from the
/// mirror (never materialized), each carrying its origin label:
/// helpscout_mirror / deterministic_local / ai_derived / human_local (for
/// the human-asserted provenance tables). Branch map by relation:
///
/// * belongs_to           customer -> organization         (customers.organization_id)
/// * involves             conversation -> customer         (conversations.customer_id)
/// * assigned_to          conversation -> agent           (conversations.assignee_id)
/// * owns                 incident -> agent                (incidents.owner_user_local_id)
/// * linked_to_issue      conversation -> known_issue      (known_issue_links, per-row ai/human)
/// * clustered_into       conversation -> issue_cluster    (issue_cluster_members)
/// * promoted_to_issue    issue_cluster -> known_issue     (issue_clusters.known_issue_id)
/// * affected_by          conversation -> incident          (incident_conversations)
/// * related_to           incident -> {kind}               (incident_related)
/// * linked_to            custom_object -> {kind}          (custom_object_links)
/// * sent_to              campaign -> customer             (outreach_recipients)
/// * generated_conversation campaign -> conversation       (outreach_recipients.hs_conversation_remote_id)
/// * cites                conversation -> knowledge_document (ai_sources via ai_runs)
/// * collaborated_on      agent -> conversation            (side_thread_participants + side_threads)
/// * about_product        incident|known_issue|issue_cluster -> product (product columns, by name)
/// * about_product        conversation -> product          (ai_attributes attribute='product')
/// * gap_evidence         knowledge_document -> knowledge_document (gap candidates' related_document_ids)
/// * human_edge           {kind} -> {kind}                 (graph_edges — the human store)
///
/// Connector rows carry no derived edges by design.
fn derived_edges_touching(
    conn: &Connection,
    kind: GraphNodeKind,
    local_id: i64,
) -> Result<Vec<DerivedEdge>> {
    // Branch row cap per direction — the envelope stays bounded on hubs
    // while `total_edges` keeps counting everything collected.
    const BRANCH_CAP: i64 = GRAPH_MAX_NEIGHBOR_EDGES as i64;
    let mut out: Vec<DerivedEdge> = Vec::new();
    let mut push = |e: DerivedEdge| out.push(e);

    let one_i64 = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> Option<i64> {
        conn.query_row(sql, p, |r| r.get(0)).ok()
    };
    let many_i64 = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> Vec<i64> {
        conn.prepare(sql)
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map(p, |r| r.get::<_, i64>(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .ok()
            })
            .unwrap_or_default()
    };
    // products are matched by NAME (the mirror stores names).
    let product_id_by_name = |name: &str| -> Option<i64> {
        conn.query_row(
            "SELECT id FROM products WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |r| r.get(0),
        )
        .ok()
    };
    let mirror = |relation: &'static str,
                  source_kind: GraphNodeKind,
                  source_id: i64,
                  target_kind: GraphNodeKind,
                  target_id: i64| {
        DerivedEdge {
            relation,
            origin: "helpscout_mirror",
            source_kind,
            source_id,
            target_kind,
            target_id,
            note: None,
            at: None,
        }
    };

    match kind {
        GraphNodeKind::Customer => {
            // belongs_to out: the customer's organization.
            if let Some(org) = one_i64(
                "SELECT organization_id FROM customers
                 WHERE id = ?1 AND deleted_at IS NULL AND organization_id IS NOT NULL",
                &[&local_id],
            ) {
                push(mirror(
                    "belongs_to",
                    kind,
                    local_id,
                    GraphNodeKind::Organization,
                    org,
                ));
            }
            // involves in: the customer's conversations.
            for conv in many_i64(
                "SELECT id FROM conversations WHERE customer_id = ?1 AND deleted_at IS NULL
                 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "involves",
                    GraphNodeKind::Conversation,
                    conv,
                    kind,
                    local_id,
                ));
            }
            // sent_to in: campaigns that touched the customer.
            for campaign in many_i64(
                "SELECT campaign_id FROM outreach_recipients
                 WHERE customer_local_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "sent_to",
                    GraphNodeKind::Campaign,
                    campaign,
                    kind,
                    local_id,
                ));
            }
        }
        GraphNodeKind::Organization => {
            // belongs_to in: the organization's customers.
            for cust in many_i64(
                "SELECT id FROM customers WHERE organization_id = ?1 AND deleted_at IS NULL
                 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "belongs_to",
                    GraphNodeKind::Customer,
                    cust,
                    kind,
                    local_id,
                ));
            }
        }
        GraphNodeKind::Conversation => {
            // involves out: the conversation's customer.
            if let Some(cust) = one_i64(
                "SELECT customer_id FROM conversations
                 WHERE id = ?1 AND deleted_at IS NULL AND customer_id IS NOT NULL",
                &[&local_id],
            ) {
                push(mirror(
                    "involves",
                    kind,
                    local_id,
                    GraphNodeKind::Customer,
                    cust,
                ));
            }
            // assigned_to out: the conversation's assignee.
            if let Some(agent) = one_i64(
                "SELECT assignee_id FROM conversations
                 WHERE id = ?1 AND deleted_at IS NULL AND assignee_id IS NOT NULL",
                &[&local_id],
            ) {
                push(mirror(
                    "assigned_to",
                    kind,
                    local_id,
                    GraphNodeKind::Agent,
                    agent,
                ));
            }
            // linked_to_issue out: per-row ai/human provenance.
            {
                let mut stmt = conn.prepare(
                    "SELECT known_issue_id, link_type FROM known_issue_links
                     WHERE conversation_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
                })?;
                for row in rows {
                    let (issue, link_type) = row?;
                    let origin = match link_type.as_deref() {
                        Some("human") => "human_local",
                        _ => "ai_derived",
                    };
                    push(DerivedEdge {
                        relation: "linked_to_issue",
                        origin,
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: GraphNodeKind::KnownIssue,
                        target_id: issue,
                        note: None,
                        at: None,
                    });
                }
            }
            // clustered_into out.
            for cluster in many_i64(
                "SELECT cluster_id FROM issue_cluster_members WHERE conversation_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(DerivedEdge {
                    relation: "clustered_into",
                    origin: "deterministic_local",
                    source_kind: kind,
                    source_id: local_id,
                    target_kind: GraphNodeKind::IssueCluster,
                    target_id: cluster,
                    note: None,
                    at: None,
                });
            }
            // affected_by out.
            for incident in many_i64(
                "SELECT incident_id FROM incident_conversations WHERE conversation_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(DerivedEdge {
                    relation: "affected_by",
                    origin: "deterministic_local",
                    source_kind: kind,
                    source_id: local_id,
                    target_kind: GraphNodeKind::Incident,
                    target_id: incident,
                    note: None,
                    at: None,
                });
            }
            // cites out: knowledge documents cited by the conversation's AI runs.
            for doc in many_i64(
                "SELECT s.source_id FROM ai_sources s
                 JOIN ai_runs r ON r.id = s.run_id
                 WHERE r.conversation_id = ?1 AND s.source_type = 'knowledge_document' LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(DerivedEdge {
                    relation: "cites",
                    origin: "ai_derived",
                    source_kind: kind,
                    source_id: local_id,
                    target_kind: GraphNodeKind::KnowledgeDocument,
                    target_id: doc,
                    note: None,
                    at: None,
                });
            }
            // about_product out (AI attributes, by product name).
            {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT value FROM ai_attributes
                     WHERE conversation_id = ?1 AND attribute = 'product'
                       AND superseded_at IS NULL AND value_type = 'text' LIMIT ?2",
                )?;
                let names =
                    stmt.query_map(params![local_id, BRANCH_CAP], |r| r.get::<_, String>(0))?;
                for name in names {
                    if let Some(product) = product_id_by_name(&name?) {
                        push(DerivedEdge {
                            relation: "about_product",
                            origin: "ai_derived",
                            source_kind: kind,
                            source_id: local_id,
                            target_kind: GraphNodeKind::Product,
                            target_id: product,
                            note: None,
                            at: None,
                        });
                    }
                }
            }
            // generated_conversation in: the campaign that generated it.
            if let Some(campaign) = one_i64(
                "SELECT r.campaign_id FROM outreach_recipients r
                 JOIN conversations cv ON cv.remote_id = r.hs_conversation_remote_id
                 WHERE cv.id = ?1 AND r.hs_conversation_remote_id IS NOT NULL",
                &[&local_id],
            ) {
                push(mirror(
                    "generated_conversation",
                    GraphNodeKind::Campaign,
                    campaign,
                    kind,
                    local_id,
                ));
            }
            // collaborated_on in: agents on the conversation's side threads.
            {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT stp.user_local_id, st.created_at
                     FROM side_thread_participants stp
                     JOIN side_threads st ON st.id = stp.side_thread_id
                     WHERE st.conversation_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
                })?;
                for row in rows {
                    let (agent, created_at) = row?;
                    push(DerivedEdge {
                        relation: "collaborated_on",
                        origin: "human_local",
                        source_kind: GraphNodeKind::Agent,
                        source_id: agent,
                        target_kind: kind,
                        target_id: local_id,
                        note: None,
                        at: created_at,
                    });
                }
            }
        }
        GraphNodeKind::KnownIssue => {
            // about_product out (deterministic, by name).
            let product_name: Option<String> = conn
                .query_row(
                    "SELECT product FROM known_issues
                     WHERE id = ?1 AND product IS NOT NULL AND TRIM(product) != ''",
                    params![local_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(name) = product_name {
                if let Some(product) = product_id_by_name(&name) {
                    push(DerivedEdge {
                        relation: "about_product",
                        origin: "deterministic_local",
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: GraphNodeKind::Product,
                        target_id: product,
                        note: None,
                        at: None,
                    });
                }
            }
            // linked_to_issue in (per-row provenance).
            {
                let mut stmt = conn.prepare(
                    "SELECT conversation_id, link_type FROM known_issue_links
                     WHERE known_issue_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
                })?;
                for row in rows {
                    let (conv, link_type) = row?;
                    let origin = match link_type.as_deref() {
                        Some("human") => "human_local",
                        _ => "ai_derived",
                    };
                    push(DerivedEdge {
                        relation: "linked_to_issue",
                        origin,
                        source_kind: GraphNodeKind::Conversation,
                        source_id: conv,
                        target_kind: kind,
                        target_id: local_id,
                        note: None,
                        at: None,
                    });
                }
            }
            // promoted_to_issue in: clusters promoted into the issue.
            for cluster in many_i64(
                "SELECT id FROM issue_clusters WHERE known_issue_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "promoted_to_issue",
                    GraphNodeKind::IssueCluster,
                    cluster,
                    kind,
                    local_id,
                ));
            }
        }
        GraphNodeKind::IssueCluster => {
            // promoted_to_issue out.
            if let Some(issue) = one_i64(
                "SELECT known_issue_id FROM issue_clusters
                 WHERE id = ?1 AND known_issue_id IS NOT NULL",
                &[&local_id],
            ) {
                push(mirror(
                    "promoted_to_issue",
                    kind,
                    local_id,
                    GraphNodeKind::KnownIssue,
                    issue,
                ));
            }
            // about_product out.
            let product_name: Option<String> = conn
                .query_row(
                    "SELECT product FROM issue_clusters
                     WHERE id = ?1 AND product IS NOT NULL AND TRIM(product) != ''",
                    params![local_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(name) = product_name {
                if let Some(product) = product_id_by_name(&name) {
                    push(DerivedEdge {
                        relation: "about_product",
                        origin: "deterministic_local",
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: GraphNodeKind::Product,
                        target_id: product,
                        note: None,
                        at: None,
                    });
                }
            }
            // clustered_into in: the cluster's conversations.
            for conv in many_i64(
                "SELECT conversation_id FROM issue_cluster_members WHERE cluster_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(DerivedEdge {
                    relation: "clustered_into",
                    origin: "deterministic_local",
                    source_kind: GraphNodeKind::Conversation,
                    source_id: conv,
                    target_kind: kind,
                    target_id: local_id,
                    note: None,
                    at: None,
                });
            }
        }
        GraphNodeKind::Incident => {
            // owns out.
            if let Some(agent) = one_i64(
                "SELECT owner_user_local_id FROM incidents
                 WHERE id = ?1 AND owner_user_local_id IS NOT NULL",
                &[&local_id],
            ) {
                push(mirror("owns", kind, local_id, GraphNodeKind::Agent, agent));
            }
            // related_to out: the incident's explicit targets.
            {
                let mut stmt = conn.prepare(
                    "SELECT target_kind, target_local_id, note FROM incident_related
                     WHERE incident_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })?;
                for row in rows {
                    let (target_kind, target_id, note) = row?;
                    let Some(tk) = GraphNodeKind::parse(&target_kind) else {
                        continue;
                    };
                    push(DerivedEdge {
                        relation: "related_to",
                        origin: "human_local",
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: tk,
                        target_id,
                        note,
                        at: None,
                    });
                }
            }
            // about_product out.
            let product_name: Option<String> = conn
                .query_row(
                    "SELECT product FROM incidents
                     WHERE id = ?1 AND product IS NOT NULL AND TRIM(product) != ''",
                    params![local_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(name) = product_name {
                if let Some(product) = product_id_by_name(&name) {
                    push(DerivedEdge {
                        relation: "about_product",
                        origin: "deterministic_local",
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: GraphNodeKind::Product,
                        target_id: product,
                        note: None,
                        at: None,
                    });
                }
            }
            // affected_by in: the incident's conversations.
            for conv in many_i64(
                "SELECT conversation_id FROM incident_conversations WHERE incident_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(DerivedEdge {
                    relation: "affected_by",
                    origin: "deterministic_local",
                    source_kind: GraphNodeKind::Conversation,
                    source_id: conv,
                    target_kind: kind,
                    target_id: local_id,
                    note: None,
                    at: None,
                });
            }
        }
        GraphNodeKind::KnowledgeDocument => {
            // cites in: conversations citing the document.
            for conv in many_i64(
                "SELECT r.conversation_id FROM ai_sources s
                 JOIN ai_runs r ON r.id = s.run_id
                 WHERE s.source_type = 'knowledge_document' AND s.source_id = ?1
                   AND r.conversation_id IS NOT NULL LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(DerivedEdge {
                    relation: "cites",
                    origin: "ai_derived",
                    source_kind: GraphNodeKind::Conversation,
                    source_id: conv,
                    target_kind: kind,
                    target_id: local_id,
                    note: None,
                    at: None,
                });
            }
            // gap_evidence out: co-cited sibling documents on knowledge-gap
            // candidates (symmetric co-citation, emitted from the center).
            {
                let mut stmt = conn.prepare(
                    "SELECT related_document_ids FROM knowledge_gap_candidates
                     WHERE related_document_ids NOT IN ('[]', '')",
                )?;
                let lists = stmt.query_map([], |r| r.get::<_, String>(0))?;
                for list in lists {
                    let list = list?;
                    if let Ok(ids) = serde_json::from_str::<Vec<i64>>(&list).or_else(|_| {
                        serde_json::from_str::<Vec<String>>(&list).map(|v| {
                            v.iter()
                                .filter_map(|s| s.trim().parse::<i64>().ok())
                                .collect::<Vec<i64>>()
                        })
                    }) {
                        if ids.contains(&local_id) {
                            for other in ids {
                                if other != local_id {
                                    push(DerivedEdge {
                                        relation: "gap_evidence",
                                        origin: "deterministic_local",
                                        source_kind: kind,
                                        source_id: local_id,
                                        target_kind: kind,
                                        target_id: other,
                                        note: None,
                                        at: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        GraphNodeKind::Agent => {
            // collaborated_on out: conversations where the agent
            // participates in a side thread.
            {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT st.conversation_id, st.created_at
                     FROM side_thread_participants stp
                     JOIN side_threads st ON st.id = stp.side_thread_id
                     WHERE stp.user_local_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
                })?;
                for row in rows {
                    let (conv, created_at) = row?;
                    push(DerivedEdge {
                        relation: "collaborated_on",
                        origin: "human_local",
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: GraphNodeKind::Conversation,
                        target_id: conv,
                        note: None,
                        at: created_at,
                    });
                }
            }
            // assigned_to in: the agent's conversations.
            for conv in many_i64(
                "SELECT id FROM conversations WHERE assignee_id = ?1 AND deleted_at IS NULL LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "assigned_to",
                    GraphNodeKind::Conversation,
                    conv,
                    kind,
                    local_id,
                ));
            }
            // owns in: the agent's incidents.
            for incident in many_i64(
                "SELECT id FROM incidents WHERE owner_user_local_id = ?1 LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "owns",
                    GraphNodeKind::Incident,
                    incident,
                    kind,
                    local_id,
                ));
            }
        }
        GraphNodeKind::Campaign => {
            // sent_to out (with the send timestamp when present).
            {
                let mut stmt = conn.prepare(
                    "SELECT customer_local_id, sent_at FROM outreach_recipients
                     WHERE campaign_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
                })?;
                for row in rows {
                    let (cust, sent_at) = row?;
                    push(DerivedEdge {
                        relation: "sent_to",
                        origin: "helpscout_mirror",
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: GraphNodeKind::Customer,
                        target_id: cust,
                        note: None,
                        at: sent_at,
                    });
                }
            }
            // generated_conversation out (remote id -> local conversation).
            for conv in many_i64(
                "SELECT cv.id FROM outreach_recipients r
                 JOIN conversations cv ON cv.remote_id = r.hs_conversation_remote_id
                 WHERE r.campaign_id = ?1 AND r.hs_conversation_remote_id IS NOT NULL LIMIT ?2",
                &[&local_id, &BRANCH_CAP],
            ) {
                push(mirror(
                    "generated_conversation",
                    kind,
                    local_id,
                    GraphNodeKind::Conversation,
                    conv,
                ));
            }
        }
        GraphNodeKind::Product => {
            // about_product in (deterministic, by name).
            let product_name: Option<String> = conn
                .query_row(
                    "SELECT name FROM products WHERE id = ?1",
                    params![local_id],
                    |r| r.get(0),
                )
                .ok();
            if let Some(name) = product_name {
                for sql in [
                    "SELECT id FROM incidents WHERE product = ?1 AND product IS NOT NULL AND TRIM(product) != ''",
                    "SELECT id FROM known_issues WHERE product = ?1 AND product IS NOT NULL AND TRIM(product) != ''",
                    "SELECT id FROM issue_clusters WHERE product = ?1 AND product IS NOT NULL AND TRIM(product) != ''",
                ] {
                    for source in many_i64(
                        &format!("{sql} LIMIT {BRANCH_CAP}"),
                        &[&name],
                    ) {
                        let source_kind = if sql.contains("incidents") {
                            GraphNodeKind::Incident
                        } else if sql.contains("known_issues") {
                            GraphNodeKind::KnownIssue
                        } else {
                            GraphNodeKind::IssueCluster
                        };
                        push(DerivedEdge {
                            relation: "about_product",
                            origin: "deterministic_local",
                            source_kind,
                            source_id: source,
                            target_kind: kind,
                            target_id: local_id,
                            note: None,
                            at: None,
                        });
                    }
                }
                // about_product in (AI attributes on conversations).
                for conv in many_i64(
                    "SELECT DISTINCT conversation_id FROM ai_attributes
                     WHERE attribute = 'product' AND superseded_at IS NULL AND value_type = 'text'
                       AND value = ?1 LIMIT ?2",
                    &[&name, &BRANCH_CAP],
                ) {
                    push(DerivedEdge {
                        relation: "about_product",
                        origin: "ai_derived",
                        source_kind: GraphNodeKind::Conversation,
                        source_id: conv,
                        target_kind: kind,
                        target_id: local_id,
                        note: None,
                        at: None,
                    });
                }
            }
        }
        GraphNodeKind::CustomObject => {
            // linked_to out: the object's explicit links.
            {
                let mut stmt = conn.prepare(
                    "SELECT target_kind, target_local_id, linked_by, linked_at
                     FROM custom_object_links WHERE object_id = ?1 LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![local_id, BRANCH_CAP], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })?;
                for row in rows {
                    let (target_kind, target_id, linked_by, linked_at) = row?;
                    let Some(tk) = GraphNodeKind::parse(&target_kind) else {
                        continue;
                    };
                    let origin = match linked_by.as_deref() {
                        Some("ai") => "ai_derived",
                        _ => "human_local",
                    };
                    push(DerivedEdge {
                        relation: "linked_to",
                        origin,
                        source_kind: kind,
                        source_id: local_id,
                        target_kind: tk,
                        target_id,
                        note: None,
                        at: linked_at,
                    });
                }
            }
        }
        GraphNodeKind::ConnectorData => {
            // No derived edges by design (rows are keyed only by row_key);
            // humans can link them explicitly.
        }
    }

    // Cross-kind in-branches that apply to EVERY node kind:
    // related_to from incidents and linked_to from custom objects.
    {
        let mut stmt = conn.prepare(
            "SELECT incident_id, note FROM incident_related
             WHERE target_kind = ?1 AND target_local_id = ?2 LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kind.as_str(), local_id, BRANCH_CAP], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (incident, note) = row?;
            push(DerivedEdge {
                relation: "related_to",
                origin: "human_local",
                source_kind: GraphNodeKind::Incident,
                source_id: incident,
                target_kind: kind,
                target_id: local_id,
                note,
                at: None,
            });
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT object_id, linked_by, linked_at FROM custom_object_links
             WHERE target_kind = ?1 AND target_local_id = ?2 LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kind.as_str(), local_id, BRANCH_CAP], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (object, linked_by, linked_at) = row?;
            let origin = match linked_by.as_deref() {
                Some("ai") => "ai_derived",
                _ => "human_local",
            };
            push(DerivedEdge {
                relation: "linked_to",
                origin,
                source_kind: GraphNodeKind::CustomObject,
                source_id: object,
                target_kind: kind,
                target_id: local_id,
                note: None,
                at: linked_at,
            });
        }
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
// GR-03: the read layer (reference graphService stats / search /
// neighbors / subgraph, ported to the port's documented renames)

/// Bounded neighbor serving (reference shared/graph.ts:154).
pub const GRAPH_MAX_NEIGHBOR_EDGES: usize = 200;
/// BFS node cap (shared/graph.ts:155).
pub const GRAPH_MAX_SUBGRAPH_NODES: usize = 250;
/// BFS depth cap (shared/graph.ts:156).
pub const GRAPH_MAX_SUBGRAPH_DEPTH: u32 = 2;
/// Per-kind search cap (shared/graph.ts:157).
pub const GRAPH_MAX_SEARCH_PER_KIND: usize = 10;
/// Total search cap (graphService.ts:803).
pub const GRAPH_MAX_SEARCH_TOTAL: usize = 80;

/// The reference kind labels (shared/graph.ts GRAPH_NODE_KIND_LABELS).
fn kind_label(kind: GraphNodeKind) -> &'static str {
    match kind {
        GraphNodeKind::Customer => "Customer",
        GraphNodeKind::Organization => "Organization",
        GraphNodeKind::Conversation => "Conversation",
        GraphNodeKind::KnownIssue => "Known issue",
        GraphNodeKind::IssueCluster => "Issue cluster",
        GraphNodeKind::Incident => "Incident",
        GraphNodeKind::KnowledgeDocument => "Knowledge document",
        GraphNodeKind::Agent => "Agent",
        GraphNodeKind::Campaign => "Campaign",
        GraphNodeKind::Product => "Product",
        GraphNodeKind::CustomObject => "Custom object",
        GraphNodeKind::ConnectorData => "Connector row",
    }
}

/// `stats()` — live counts over the local mirror (reference
/// graphService.ts:672-724): per-kind node counts and per-relation edge
/// counts (origin labeled), plus the human-edge total. Port renames:
/// known_issue_conversations→known_issue_links,
/// issue_cluster_conversations→issue_cluster_members,
/// knowledge_candidates→knowledge_gap_candidates,
/// support_graph_edges→graph_edges, customer_local_id→customer_id,
/// assignee_local_id→assignee_id (DB-04).
pub fn graph_stats(conn: &Connection) -> Result<Value> {
    let count = |sql: &str| -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) AS n FROM ({sql})"), [], |r| {
            r.get(0)
        })
        .unwrap_or(0)
    };
    let node_specs: [(&str, &str); 12] = [
        (
            "customer",
            "SELECT id FROM customers WHERE deleted_at IS NULL",
        ),
        (
            "organization",
            "SELECT id FROM organizations WHERE deleted_at IS NULL",
        ),
        (
            "conversation",
            "SELECT id FROM conversations WHERE deleted_at IS NULL",
        ),
        ("known_issue", "SELECT id FROM known_issues"),
        ("issue_cluster", "SELECT id FROM issue_clusters"),
        ("incident", "SELECT id FROM incidents"),
        ("knowledge_document", "SELECT id FROM knowledge_documents"),
        ("agent", "SELECT id FROM users WHERE deleted_at IS NULL"),
        ("campaign", "SELECT id FROM outreach_campaigns"),
        ("product", "SELECT id FROM products"),
        (
            "custom_object",
            "SELECT id FROM custom_objects WHERE deleted_at IS NULL",
        ),
        ("connector_data", "SELECT id FROM connector_rows"),
    ];
    let nodes: Vec<Value> = node_specs
        .iter()
        .map(|(kind, sql)| {
            let kind = GraphNodeKind::ALL
                .into_iter()
                .find(|k| k.as_str() == *kind)
                .expect("12 kinds");
            json!({"kind": kind.as_str(), "label": kind_label(kind), "count": count(sql)})
        })
        .collect();
    let edge_specs: [(&str, &str, &str); 18] = [
        ("belongs_to", "helpscout_mirror", "SELECT 1 FROM customers cu JOIN organizations o ON o.id = cu.organization_id"),
        ("involves", "helpscout_mirror", "SELECT 1 FROM conversations c WHERE c.customer_id IS NOT NULL AND c.deleted_at IS NULL"),
        ("assigned_to", "helpscout_mirror", "SELECT 1 FROM conversations c WHERE c.assignee_id IS NOT NULL AND c.deleted_at IS NULL"),
        ("owns", "helpscout_mirror", "SELECT 1 FROM incidents i WHERE i.owner_user_local_id IS NOT NULL"),
        ("linked_to_issue", "ai_derived", "SELECT 1 FROM known_issue_links"),
        ("clustered_into", "deterministic_local", "SELECT 1 FROM issue_cluster_members"),
        ("promoted_to_issue", "helpscout_mirror", "SELECT 1 FROM issue_clusters WHERE known_issue_id IS NOT NULL"),
        ("affected_by", "deterministic_local", "SELECT 1 FROM incident_conversations"),
        ("related_to", "human_local", "SELECT 1 FROM incident_related"),
        ("linked_to", "human_local", "SELECT 1 FROM custom_object_links"),
        ("sent_to", "helpscout_mirror", "SELECT 1 FROM outreach_recipients"),
        ("generated_conversation", "helpscout_mirror", "SELECT 1 FROM outreach_recipients WHERE hs_conversation_remote_id IS NOT NULL"),
        ("cites", "ai_derived", "SELECT 1 FROM ai_sources s JOIN ai_runs r ON r.id = s.run_id WHERE s.source_type = 'knowledge_document' AND r.conversation_id IS NOT NULL"),
        ("collaborated_on", "human_local", "SELECT 1 FROM side_thread_participants stp JOIN side_threads st ON st.id = stp.side_thread_id"),
        ("about_product", "deterministic_local", "SELECT 1 FROM (SELECT 1 FROM incidents WHERE product IS NOT NULL AND TRIM(product) != '' UNION ALL SELECT 1 FROM known_issues WHERE product IS NOT NULL AND TRIM(product) != '' UNION ALL SELECT 1 FROM issue_clusters WHERE product IS NOT NULL AND TRIM(product) != '')"),
        ("about_product", "ai_derived", "SELECT 1 FROM ai_attributes WHERE attribute = 'product' AND superseded_at IS NULL AND value_type = 'text'"),
        ("gap_evidence", "deterministic_local", "SELECT 1 FROM knowledge_gap_candidates WHERE related_document_ids NOT IN ('[]', '')"),
        ("human_edge", "human_local", "SELECT 1 FROM graph_edges"),
    ];
    let edges: Vec<Value> = edge_specs
        .iter()
        .map(|(relation, origin, sql)| {
            json!({"relation": relation, "origin": origin, "count": count(sql)})
        })
        .collect();
    Ok(json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "nodes": nodes,
        "edges": edges,
        "human_edges": count("SELECT 1 FROM graph_edges"),
        "notes": [
            "Counts are live counts over the local mirror - no denormalized totals that could go stale.",
            "Connector rows carry no derived edges by design (rows are keyed only by row_key); humans can link them explicitly.",
            "linked_to_issue edges aggregate the human and AI link provenance stored per row."
        ]
    }))
}

/// Escape LIKE metacharacters (the reference's `escapeLike`).
fn escape_like(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for ch in v.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `search(query, kinds)` — LIKE-escaped bounded search across node labels
/// (reference graphService.ts:727-812): per-kind capped at 10, 80 total,
/// every branch a constant SQL string with the value as the only bound
/// parameter. Port adaptations: known-issue/cluster labels read
/// COALESCE(title, name) (the M015/M016 tables), and the incident label
/// tolerates the guarded NULL code/title columns.
pub fn graph_search(
    conn: &Connection,
    query: &str,
    kinds: Option<&[GraphNodeKind]>,
) -> Result<Vec<Value>> {
    let q: String = query.trim().chars().take(120).collect();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let like = format!("%{}%", escape_like(&q));
    // The conversation branch also matches the exact number (JS:
    // `Number.isInteger(Number(q)) ? Number(q) : -1`).
    let number: i64 = q.parse::<i64>().unwrap_or(-1);
    let wanted: Vec<GraphNodeKind> = match kinds {
        Some(list) => list.to_vec(),
        None => GraphNodeKind::ALL.to_vec(),
    };
    let per_kind = GRAPH_MAX_SEARCH_PER_KIND;
    let customer_label =
        "COALESCE(NULLIF(TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, '')), ''), 'Customer #' || cu.id)";
    let user_label =
        "COALESCE(NULLIF(TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, '')), ''), 'User #' || u.id)";
    let conversation_label =
        "'#' || c.number || ' ' || COALESCE(SUBSTR(c.subject, 1, 100), '(no subject)')";
    let incident_label =
        "COALESCE(i.code, 'INC') || ' ' || COALESCE(i.title, 'Incident #' || i.id)";
    let mut results: Vec<Value> = Vec::new();
    for kind in wanted {
        if results.len() >= GRAPH_MAX_SEARCH_TOTAL {
            break;
        }
        let (sql, number_bound) = match kind {
            GraphNodeKind::Conversation => (
                format!(
                    "SELECT c.id, {conversation_label} label, c.status sublabel, (c.deleted_at IS NOT NULL) deleted
                       FROM conversations c
                      WHERE (c.number = ?1 OR c.subject LIKE ?2 ESCAPE '\\') AND c.deleted_at IS NULL
                      LIMIT {per_kind}"
                ),
                true,
            ),
            GraphNodeKind::Customer => (
                format!(
                    "SELECT cu.id, {customer_label} label, NULL sublabel, (cu.deleted_at IS NOT NULL) deleted
                       FROM customers cu
                      WHERE ({customer_label} LIKE ?1 ESCAPE '\\' OR cu.last_name LIKE ?1 ESCAPE '\\')
                        AND cu.deleted_at IS NULL
                      LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::Organization => (
                format!(
                    "SELECT o.id, o.name label, o.domains sublabel, (o.deleted_at IS NOT NULL) deleted
                       FROM organizations o WHERE o.name LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::KnownIssue => (
                format!(
                    "SELECT ki.id, COALESCE(ki.title, ki.name) label, ki.status sublabel, 0 deleted
                       FROM known_issues ki WHERE COALESCE(ki.title, ki.name) LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::IssueCluster => (
                format!(
                    "SELECT ic.id, COALESCE(ic.title, ic.name) label, 'cluster' sublabel, 0 deleted
                       FROM issue_clusters ic WHERE COALESCE(ic.title, ic.name) LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::Incident => (
                format!(
                    "SELECT i.id, {incident_label} label, i.status sublabel, 0 deleted
                       FROM incidents i WHERE (i.title LIKE ?1 ESCAPE '\\' OR i.code LIKE ?1 ESCAPE '\\') LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::KnowledgeDocument => (
                format!(
                    "SELECT kd.id, kd.title label, kd.visibility sublabel, 0 deleted
                       FROM knowledge_documents kd WHERE kd.title LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::Agent => (
                format!(
                    "SELECT u.id, {user_label} label, u.email sublabel, (u.deleted_at IS NOT NULL) deleted
                       FROM users u WHERE ({user_label} LIKE ?1 ESCAPE '\\' OR u.email LIKE ?1 ESCAPE '\\') LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::Campaign => (
                format!(
                    "SELECT oc.id, oc.name label, oc.status sublabel, 0 deleted
                       FROM outreach_campaigns oc WHERE oc.name LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::Product => (
                format!(
                    "SELECT p.id, p.name label, p.description sublabel, 0 deleted
                       FROM products p WHERE p.name LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::CustomObject => (
                format!(
                    "SELECT co.id, co.title label, cot.name sublabel, (co.deleted_at IS NOT NULL) deleted
                       FROM custom_objects co JOIN custom_object_types cot ON cot.id = co.type_id
                      WHERE co.title LIKE ?1 ESCAPE '\\' AND co.deleted_at IS NULL LIMIT {per_kind}"
                ),
                false,
            ),
            GraphNodeKind::ConnectorData => (
                format!(
                    "SELECT cr.id, 'row ' || cr.row_key label, cn.name sublabel, 0 deleted
                       FROM connector_rows cr JOIN connectors cn ON cn.id = cr.connector_id
                      WHERE cr.row_key LIKE ?1 ESCAPE '\\' LIMIT {per_kind}"
                ),
                false,
            ),
        };
        let rows: Vec<Value> = conn
            .prepare(&sql)
            .and_then(|mut stmt| {
                let rows: Vec<rusqlite::Result<Value>> = if number_bound {
                    stmt.query_map(params![number, like], map_search_row(kind))?
                        .collect()
                } else {
                    stmt.query_map(params![like], map_search_row(kind))?
                        .collect()
                };
                Ok(rows.into_iter().filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();
        results.extend(rows);
    }
    Ok(results)
}

/// One served search row (the reference GraphSearchResult).
fn map_search_row(kind: GraphNodeKind) -> impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    move |r| {
        Ok(json!({
            "kind": kind.as_str(),
            "local_id": r.get::<_, i64>(0)?,
            "label": r.get::<_, String>(1)?,
            "sublabel": r.get::<_, Option<String>>(2)?,
            "deleted": r.get::<_, i64>(3)? != 0,
        }))
    }
}

/// `neighbors(kind, id, {direction, limit})` — the reference envelope
/// `{node, edges, total_edges, truncated, notes}` (graphService.ts:522-622).
/// The edge set is the human-asserted store PLUS the ~24 read-time derived
/// branches (`derived_edges_touching` — GR-01), both directions filtered by
/// the `direction` param. Edges are sorted by (relation, target label)
/// before the limit slice, exactly like the reference.
pub fn neighbors_json(
    conn: &Connection,
    kind: GraphNodeKind,
    id: i64,
    direction: &str,
    limit: i64,
) -> Result<Option<Value>> {
    let Some(center) = node_ref(conn, kind, id)? else {
        return Ok(None);
    };
    let center_json = serde_json::to_value(&center).unwrap_or(Value::Null);
    let limit = limit.clamp(1, GRAPH_MAX_NEIGHBOR_EDGES as i64) as usize;
    let mut raw: usize = 0;
    // (relation, target label, edge json) — the sort keys travel with the row.
    let mut edges: Vec<(String, String, Value)> = Vec::new();
    let human_ref = |kind: GraphNodeKind, id: i64| -> Value {
        node_ref(conn, kind, id)
            .ok()
            .flatten()
            .and_then(|r| serde_json::to_value(r).ok())
            .unwrap_or_else(|| {
                json!({
                    "kind": kind.as_str(),
                    "local_id": id,
                    "label": format!("#{id} (removed)"),
                    "sublabel": Value::Null,
                    "deleted": true,
                })
            })
    };
    if direction != "in" {
        let mut stmt = conn.prepare(
            "SELECT relation, note, created_at, target_kind, target_local_id
               FROM graph_edges WHERE source_kind = ?1 AND source_local_id = ?2
               LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kind.as_str(), id, GRAPH_MAX_NEIGHBOR_EDGES], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (relation, note, at, target_kind, target_local_id) = row?;
            let Some(tk) = GraphNodeKind::ALL
                .into_iter()
                .find(|k| k.as_str() == target_kind)
            else {
                continue;
            };
            raw += 1;
            let target = human_ref(tk, target_local_id);
            let label = target["label"].as_str().unwrap_or("").to_string();
            edges.push((
                relation.clone(),
                label,
                json!({
                    "relation": relation,
                    "origin": "human_local",
                    "source": center_json,
                    "target": target,
                    "note": note,
                    "at": at,
                }),
            ));
        }
    }
    if direction != "out" {
        let mut stmt = conn.prepare(
            "SELECT relation, note, created_at, source_kind, source_local_id
               FROM graph_edges WHERE target_kind = ?1 AND target_local_id = ?2
               LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kind.as_str(), id, GRAPH_MAX_NEIGHBOR_EDGES], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (relation, note, at, source_kind, source_local_id) = row?;
            let Some(sk) = GraphNodeKind::ALL
                .into_iter()
                .find(|k| k.as_str() == source_kind)
            else {
                continue;
            };
            raw += 1;
            let source = human_ref(sk, source_local_id);
            edges.push((
                relation.clone(),
                // The reference sorts by edge.target.label — for inbound
                // edges that is the center's label.
                center_json["label"].as_str().unwrap_or("").to_string(),
                json!({
                    "relation": relation,
                    "origin": "human_local",
                    "source": source,
                    "target": center_json,
                    "note": note,
                    "at": at,
                }),
            ));
        }
    }
    // The ~24 read-time derived branches (GR-01): derived edges live
    // alongside the human store; each carries its own origin label
    // (helpscout_mirror / deterministic_local / ai_derived / human_local
    // for the human-asserted provenance tables).
    for de in derived_edges_touching(conn, kind, id)? {
        let is_out = de.source_kind == kind && de.source_id == id;
        let is_in = de.target_kind == kind && de.target_id == id;
        let (outward, far) = if is_out {
            (true, (de.target_kind, de.target_id))
        } else if is_in {
            (false, (de.source_kind, de.source_id))
        } else {
            continue;
        };
        if (outward && direction == "in") || (!outward && direction == "out") {
            continue;
        }
        raw += 1;
        let far_json = human_ref(far.0, far.1);
        let (source, target, sort_label) = if outward {
            (
                center_json.clone(),
                far_json.clone(),
                far_json["label"].as_str().unwrap_or("").to_string(),
            )
        } else {
            (
                far_json.clone(),
                center_json.clone(),
                center_json["label"].as_str().unwrap_or("").to_string(),
            )
        };
        edges.push((
            de.relation.to_string(),
            sort_label,
            json!({
                "relation": de.relation,
                "origin": de.origin,
                "source": source,
                "target": target,
                "note": de.note,
                "at": de.at,
            }),
        ));
    }
    edges.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let truncated = raw > limit;
    let mut notes: Vec<String> = vec![
        "Derived edges are computed live from the local mirror - they can never drift from the data they describe.".to_string(),
        "about_product edges from conversations carry AI-attribute provenance; everything deterministic is labeled as such.".to_string(),
        "Connector rows have no derived links by design - only human-asserted edges can connect them.".to_string(),
    ];
    if truncated {
        notes.push(format!(
            "Bounded to the first {limit} of {raw} edges (deep hubs: expand from a specific neighbor)."
        ));
    }
    Ok(Some(json!({
        "node": center_json,
        "edges": edges.into_iter().take(limit).map(|(_, _, e)| e).collect::<Vec<_>>(),
        "total_edges": raw,
        "truncated": truncated,
        "notes": notes,
    })))
}

/// `subgraph(kind, id, {depth})` — the bounded BFS expansion
/// (reference graphService.ts:625-670): depth clamped to 1..2, nodes
/// capped at 250 (minimum 10), 40 edges per frontier node, the node cap
/// reported honestly through `truncated`.
pub fn subgraph_json(
    conn: &Connection,
    kind: GraphNodeKind,
    id: i64,
    depth: u32,
    max_nodes: u32,
) -> Result<Option<Value>> {
    let Some(seed) = node_ref(conn, kind, id)? else {
        return Ok(None);
    };
    let depth = depth.clamp(1, GRAPH_MAX_SUBGRAPH_DEPTH);
    let max_nodes = max_nodes.clamp(10, GRAPH_MAX_SUBGRAPH_NODES as u32) as usize;
    let seed_json = serde_json::to_value(&seed).unwrap_or(Value::Null);
    let node_key = |v: &Value| {
        format!(
            "{}:{}",
            v["kind"].as_str().unwrap_or(""),
            v["local_id"].as_i64().unwrap_or(-1)
        )
    };
    let mut nodes: Vec<Value> = vec![seed_json.clone()];
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::from([node_key(&seed_json)]);
    let mut edges: Vec<Value> = Vec::new();
    let mut frontier: Vec<Value> = vec![seed_json.clone()];
    let mut depth_reached: u32 = 0;
    for d in 0..depth {
        if nodes.len() >= max_nodes {
            break;
        }
        let mut next: Vec<Value> = Vec::new();
        for f in &frontier {
            if nodes.len() >= max_nodes {
                break;
            }
            let (Some(fk), Some(fi)) = (
                f["kind"].as_str().and_then(GraphNodeKind::parse),
                f["local_id"].as_i64(),
            ) else {
                continue;
            };
            let Some(nb) = neighbors_json(conn, fk, fi, "both", 40)? else {
                continue;
            };
            for edge in nb["edges"].as_array().cloned().unwrap_or_default() {
                edges.push(edge.clone());
                let far = if node_key(&edge["source"]) == node_key(f) {
                    edge["target"].clone()
                } else {
                    edge["source"].clone()
                };
                if !seen.contains(&node_key(&far)) {
                    if nodes.len() >= max_nodes {
                        break;
                    }
                    seen.insert(node_key(&far));
                    nodes.push(far.clone());
                    next.push(far);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
        depth_reached = d + 1;
    }
    let truncated = nodes.len() >= max_nodes;
    let edge_cap = (max_nodes * 3).min(edges.len());
    Ok(Some(json!({
        "seeds": [seed_json],
        "nodes": nodes,
        "edges": edges[..edge_cap],
        "truncated": truncated,
        "depth_reached": depth_reached,
        "notes": [
            format!("Bounded exploration: at most {depth} hop(s) and {max_nodes} nodes."),
            if truncated {
                "Node cap reached - expand from a specific neighbor instead of deepening blindly."
            } else {
                "Full expansion within bounds."
            }
        ]
    })))
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
