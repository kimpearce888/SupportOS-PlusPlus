//! Operations routes — mirrors src/server/routes/operations.ts

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/operations/center — operations center snapshot.
pub async fn center(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mailbox_id = params.get("mailboxId").and_then(|m| m.parse::<i64>().ok());
    match crate::operations::build_snapshot(&conn, mailbox_id) {
        Ok(snapshot) => {
            match serde_json::to_value(&snapshot) {
                Ok(v) => Json(v),
                Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
            }
        }
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}

/// GET /api/operations/workload
pub async fn workload(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::workload::team_workload(&conn) {
        Ok(w) => Json(serde_json::to_value(&w).unwrap_or(json!({}))),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}

/// PUT /api/operations/capacity
pub async fn set_capacity(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let capacity = body.get("capacity").and_then(|v| v.as_i64()).unwrap_or(10);
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::settings::set_i64(&conn, "team_capacity", capacity);
    Json(json!({"ok": true}))
}

/// PUT /api/operations/waiting-threshold
pub async fn set_waiting_threshold(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let minutes = body.get("minutes").and_then(|v| v.as_i64()).unwrap_or(240);
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::settings::set_i64(&conn, "waiting_threshold_minutes", minutes);
    Json(json!({"ok": true}))
}

/// GET /api/operations/suggested-assignees
pub async fn suggested_assignees(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = conn.prepare("SELECT id, first_name, last_name FROM users ORDER BY id LIMIT 10").unwrap();
    let people: Vec<Value> = stmt.query_map([], |r| {
        Ok(json!({"id": r.get::<_, i64>(0)?, "name": format!("{} {}", r.get::<_, String>(1)?, r.get::<_, String>(2)?)}))
    }).unwrap().filter_map(|r| r.ok()).collect();
    Json(json!({"assignees": people}))
}
