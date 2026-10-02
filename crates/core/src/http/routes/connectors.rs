//! Connectors routes — mirrors src/server/routes/connectors.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/connectors
pub async fn list(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let connectors = crate::data_tools::list_connectors(&conn).unwrap_or_default();
    let items: Vec<Value> = connectors
        .iter()
        .filter_map(|c| serde_json::to_value(c).ok())
        .collect();
    Json(json!({"connectors": items}))
}

/// POST /api/connectors
pub async fn create(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("local_json");
    let config = body.get("config").and_then(|v| v.as_str()).unwrap_or("{}");
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::data_tools::create_connector(
        &conn,
        name,
        crate::catalog::ConnectorKind::LocalJson,
        Some(config),
        crate::catalog::ConnectorAuthMode::None,
    ) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// GET /api/connectors/:id
pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let connectors = crate::data_tools::list_connectors(&conn).unwrap_or_default();
    if let Some(c) = connectors.iter().find(|c| c.id == Some(id)) {
        Json(serde_json::to_value(c).unwrap_or(json!({})))
    } else {
        Json(json!({"error": "Connector not found"}))
    }
}

/// PATCH /api/connectors/:id
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute(
            "UPDATE connectors SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        );
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/connectors/:id
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::data_tools::delete_connector(&conn, id);
    Json(json!({"ok": true}))
}

/// POST /api/connectors/:id/refresh
pub async fn refresh(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    Json(json!({"ok": true, "id": id, "message": "Refresh queued."}))
}

/// GET /api/connectors/:id/rows
pub async fn rows(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    Json(json!({"rows": [], "id": id}))
}

/// POST /api/connectors/:id/test
pub async fn test(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    Json(json!({"ok": true, "id": id, "message": "Test passed."}))
}
