//! Graph routes — mirrors src/server/routes/graph.ts (v2.2.0, plan Phase 34).
//!
//! GR-02: the human-edge half adopts the reference wire contract — the
//! closed 5-relation vocabulary on meta, the reference 422/404/409
//! envelopes on POST/DELETE, endpoint checks (self edge / source 404 /
//! target 404 / duplicate), and the {edges, total} listing. The
//! read-derivation half (stats per-kind breakdown, capped search, the
//! direction/limit params on neighbors, bounded BFS subgraph) is plan items
//! GR-03/GR-01 and keeps the port's interim envelopes below.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::super::server::AppState;
use crate::catalog::{GraphHumanRelation, GraphNodeKind};

/// GET /api/graph/stats
///
/// Interim shape (flat counts); the reference per-kind/per-relation
/// breakdown lands with GR-03. `human_edges` now counts the human-edge
/// store (the reference's `support_graph_edges` row count) — every stored
/// edge is human-asserted until the derived read-time layer exists.
pub async fn stats(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let nodes: i64 = conn
        .query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))
        .unwrap_or(0);
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))
        .unwrap_or(0);
    Json(json!({
        "nodes": nodes,
        "edges": edges,
        "human_edges": edges,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "notes": "Graph stats generated from local SQLite graph_nodes/graph_edges tables. Stored edges are human-asserted (reference support_graph_edges contract); the derived read-time layer lands with GR-01/GR-03."
    }))
}

/// GET /api/graph/meta — the reference contract: the 12-kind union, the
/// closed 5-relation human vocabulary, and the reference's notes array.
pub async fn meta(State(_state): State<AppState>) -> Json<Value> {
    let kinds: Vec<&str> = GraphNodeKind::ALL.iter().map(|k| k.as_str()).collect();
    let relations: Vec<&str> = GraphHumanRelation::ALL.iter().map(|r| r.as_str()).collect();
    Json(json!({
        "node_kinds": kinds,
        "human_relations": relations,
        "notes": [
            "Derived edges are computed live from the local mirror; only human-asserted edges are stored.",
            "Connector rows have no derived links by design."
        ]
    }))
}

/// GET /api/graph/search
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let query = params.get("q").cloned().unwrap_or_default();
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let nodes: Vec<Value> = conn
        .prepare("SELECT id, kind, label, properties_json FROM graph_nodes WHERE label LIKE ?1 ORDER BY id LIMIT 20")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![format!("%{query}%")], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "label": r.get::<_, Option<String>>(2)?,
                    "properties": r.get::<_, Option<String>>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"results": nodes}))
}

/// GET /api/graph/node/:kind/:id
pub async fn node(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, kind, label, properties_json FROM graph_nodes WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "kind": r.get::<_, String>(1)?,
                "label": r.get::<_, Option<String>>(2)?,
                "properties": r.get::<_, Option<String>>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Node not found"})),
    }
}

/// GET /api/graph/neighbors/:kind/:id — interim envelope ({neighbors}).
///
/// Addresses the node by (kind, local mirror id) like the reference; the
/// items are reference-shaped `GraphNodeRef`s of the opposite endpoints of
/// the stored human edges (both directions). The reference's direction /
/// limit params, derived branches and 404 envelope land with GR-01/GR-03.
pub async fn neighbors(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::reports::get_graph_neighbors(&conn, &kind, id) {
        Ok(refs) => Json(json!({ "neighbors": refs })),
        Err(e) => Json(json!({"error": e.to_string(), "neighbors": []})),
    }
}

/// GET /api/graph/subgraph/:kind/:id — interim envelope ({nodes, edges})
/// over the center ref plus the stored human edges (both directions). The
/// reference's bounded BFS (depth <= 2, node cap) lands with GR-03.
pub async fn subgraph(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let kind_enum = crate::reports::validate_graph_node_kind(&kind).ok();
    let center: Option<Value> = kind_enum
        .and_then(|k| crate::support_graph::node_ref(&conn, k, id).ok().flatten())
        .and_then(|r| serde_json::to_value(r).ok());
    let neighbors: Vec<Value> = crate::reports::get_graph_neighbors(&conn, &kind, id)
        .unwrap_or_default()
        .iter()
        .filter_map(|n| serde_json::to_value(n).ok())
        .collect();
    let edges = kind_enum
        .map(|k| crate::support_graph::human_edges_touching(&conn, k, id).unwrap_or_default())
        .unwrap_or_default();
    let mut nodes = Vec::with_capacity(1 + neighbors.len());
    if let Some(c) = center {
        nodes.push(c);
    }
    nodes.extend(neighbors);
    Json(json!({"nodes": nodes, "edges": edges}))
}

/// GET /api/graph/edges — the reference contract (graph.ts:129-134):
/// `limit = Math.min(200, Math.max(1, Number(q.limit ?? 50) || 50))` and
/// `offset = Math.max(0, Number(q.offset ?? 0) || 0)` — blank/garbage/0
/// fall back to the defaults exactly like JS falsy semantics — serving
/// `{edges: [GraphHumanEdge], total}`.
pub async fn list_edges(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let limit = js_number_or(&params, "limit", 50.0);
    let limit = limit.clamp(1.0, 200.0) as i64;
    let offset = js_number_or(&params, "offset", 0.0).max(0.0) as i64;

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let (edges, total) =
        crate::support_graph::list_human_edges(&conn, limit, offset).unwrap_or_default();
    Json(json!({
        "edges": edges.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
        "total": total,
    }))
}

/// JS `Number(q.x ?? d) || d`: absent, blank, garbage or 0 fall back to the
/// default (0 and NaN are falsy in the reference's `||`).
fn js_number_or(params: &HashMap<String, String>, key: &str, default: f64) -> f64 {
    match params.get(key) {
        None => default,
        Some(raw) => {
            let n = crate::conversation_ops::js_number(raw).unwrap_or(f64::NAN);
            if n == 0.0 || n.is_nan() {
                default
            } else {
                n
            }
        }
    }
}

/// POST /api/graph/edges — assert a human edge (the reference contract).
///
/// Validation failures surface the reference Zod-global envelope (422,
/// `Invalid request (path): message`, issues array); then the endpoint
/// checks run in the reference's fixed order — self edge → 409, source
/// missing → 404, target missing → 404, duplicate → 409 — and a
/// successful link returns `{ok: true, edge}`.
pub async fn create_edge(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let input = match crate::support_graph::validate_link_body(&body) {
        Ok(input) => input,
        Err(issues) => {
            let refs: Vec<(&str, &str)> = issues
                .iter()
                .map(|(path, message)| (*path, message.as_str()))
                .collect();
            return crate::conversation_ops::zod_422_multi(&refs);
        }
    };

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::support_graph::link_human_edge(&conn, &input) {
        Ok(edge) => Json(json!({"ok": true, "edge": edge.to_json()})).into_response(),
        Err(crate::support_graph::LinkError::SelfEdge) => (
            StatusCode::CONFLICT,
            Json(json!({
                "statusCode": 409,
                "error": "Conflict",
                "message": "A node cannot be linked to itself."
            })),
        )
            .into_response(),
        Err(crate::support_graph::LinkError::SourceNotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Source node not found."
            })),
        )
            .into_response(),
        Err(crate::support_graph::LinkError::TargetNotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Target node not found."
            })),
        )
            .into_response(),
        Err(crate::support_graph::LinkError::Duplicate) => (
            StatusCode::CONFLICT,
            Json(json!({
                "statusCode": 409,
                "error": "Conflict",
                "message": "This edge already exists (duplicate)."
            })),
        )
            .into_response(),
    }
}

/// DELETE /api/graph/edges/:id — the reference contract: non-positive ids
/// are 422s, unknown ids are 404s, and only a real removal answers
/// `{ok: true}` (the old route always claimed success).
pub async fn delete_edge(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    if id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "Edge id must be a positive integer."
            })),
        )
            .into_response();
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::support_graph::unlink_human_edge(&conn, id) {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Human edge not found."
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string()
            })),
        )
            .into_response(),
    }
}
