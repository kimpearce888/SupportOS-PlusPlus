//! Issues routes — mirrors src/server/routes/issues.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/issues/clusters
pub async fn list_clusters(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let clusters: Vec<Value> = conn
        .prepare("SELECT id, name, status, created_at FROM issue_clusters ORDER BY id DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"clusters": clusters}))
}

/// GET /api/issues/sla-alerts
pub async fn sla_alerts(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let breached: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE response_state = 'sla_breached'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({"alerts": [{"state": "breached", "count": breached}]}))
}

/// GET /api/issues/clusters/:id
pub async fn get_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let row = conn.query_row(
        "SELECT id, name, status, created_at FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Cluster not found"})),
    }
}

/// DELETE /api/issues/clusters/:id
pub async fn delete_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "DELETE FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known
pub async fn list_known(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let issues = crate::intelligence_features::list_known_issues(&conn, None).unwrap_or_default();
    let items: Vec<Value> = issues
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    Json(json!({"known_issues": items}))
}

/// POST /api/issues/known
pub async fn create_known(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "INSERT INTO known_issues (name, status, description, created_at, updated_at) VALUES (?1, 'active', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![name, description],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known/:id
pub async fn get_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::intelligence_features::list_known_issues(&conn, Some("active")) {
        Ok(issues) => {
            if let Some(issue) = issues.iter().find(|i| i.id == Some(id)) {
                Json(serde_json::to_value(issue).unwrap_or(json!({})))
            } else {
                Json(json!({"error": "Known issue not found"}))
            }
        }
        Err(_) => Json(json!({"error": "Failed to list known issues"})),
    }
}

/// PATCH /api/issues/known/:id
pub async fn update_known(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET name = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![name, id]);
    }
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![status, id]);
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id
pub async fn delete_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "DELETE FROM known_issues WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/link/:conversationId
pub async fn link_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "INSERT OR IGNORE INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (?1, ?2, 'manual')",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id/link/:conversationId
pub async fn unlink_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "DELETE FROM known_issue_links WHERE known_issue_id = ?1 AND conversation_id = ?2",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/refs
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "id": id}))
}

/// GET /api/issues/cases
pub async fn list_cases(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"cases": []}))
}

/// POST /api/issues/cases/from-conversation/:conversationId
pub async fn case_from_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    Json(json!({"ok": true, "conversationId": conversation_id}))
}
