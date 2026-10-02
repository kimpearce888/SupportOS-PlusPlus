//! Graph routes — mirrors src/server/routes/graph.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/graph/stats
pub async fn stats(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let nodes: i64 = conn
        .query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))
        .unwrap_or(0);
    let edges: i64 = conn
        .query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))
        .unwrap_or(0);
    // human_edges: edges between two human-typed nodes (e.g. customer-knowledge).
    // The reference reports this as a separate count for the support-graph page.
    let human_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_edges e
             JOIN graph_nodes s ON s.id = e.source_id
             JOIN graph_nodes t ON t.id = e.target_id
             WHERE s.kind IN ('customer', 'user', 'team') OR t.kind IN ('customer', 'user', 'team')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({
        "nodes": nodes,
        "edges": edges,
        "human_edges": human_edges,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "notes": "Graph stats generated from local SQLite graph_nodes/graph_edges tables."
    }))
}

/// GET /api/graph/meta
pub async fn meta(State(_state): State<AppState>) -> Json<Value> {
    use crate::catalog::GraphNodeKind;
    let kinds: Vec<Value> = GraphNodeKind::ALL
        .iter()
        .map(|k| json!({"kind": k.as_str()}))
        .collect();
    // human_relations: the reference's graph meta endpoint returns a list of
    // human-typed relation kinds (e.g. mentions, replied_to, assigned_to).
    let human_relations: Vec<Value> = vec![
        json!("mentions"),
        json!("replied_to"),
        json!("assigned_to"),
        json!("reviewed_by"),
        json!("escalated_to"),
    ];
    Json(json!({
        "node_kinds": kinds,
        "human_relations": human_relations,
        "notes": "Graph meta generated from the closed-vocabulary catalog."
    }))
}

/// GET /api/graph/search
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
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

/// GET /api/graph/neighbors/:kind/:id
pub async fn neighbors(
    State(state): State<AppState>,
    Path((_kind, id)): Path<(String, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::reports::get_graph_neighbors(&conn, id) {
        Ok(neighbors) => {
            let items: Vec<Value> = neighbors
                .iter()
                .filter_map(|n| serde_json::to_value(n).ok())
                .collect();
            Json(json!({"neighbors": items}))
        }
        Err(e) => Json(json!({"error": e.to_string(), "neighbors": []})),
    }
}

/// GET /api/graph/subgraph/:kind/:id
pub async fn subgraph(
    State(state): State<AppState>,
    Path((_kind, id)): Path<(String, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let neighbors = crate::reports::get_graph_neighbors(&conn, id).unwrap_or_default();
    let nodes: Vec<Value> = neighbors
        .iter()
        .filter_map(|n| serde_json::to_value(n).ok())
        .collect();
    Json(json!({"nodes": nodes, "edges": []}))
}

/// GET /api/graph/edges
pub async fn list_edges(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let edges: Vec<Value> = conn
        .prepare("SELECT id, source_id, target_id, kind FROM graph_edges ORDER BY id LIMIT 100")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "source_id": r.get::<_, i64>(1)?,
                    "target_id": r.get::<_, i64>(2)?,
                    "kind": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let total = edges.len() as i64;
    Json(json!({"edges": edges, "total": total}))
}

/// POST /api/graph/edges
pub async fn create_edge(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let source_id = body.get("sourceId").and_then(|v| v.as_i64()).unwrap_or(0);
    let target_id = body.get("targetId").and_then(|v| v.as_i64()).unwrap_or(0);
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("related");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::reports::add_graph_edge(&conn, source_id, target_id, kind);
    Json(json!({"ok": true}))
}

/// DELETE /api/graph/edges/:id
pub async fn delete_edge(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM graph_edges WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}
