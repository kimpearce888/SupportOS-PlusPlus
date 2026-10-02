//! Automation routes — mirrors src/server/routes/automation.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/automation/rules
pub async fn list_rules(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let rules = crate::automation::list_rules(&conn).unwrap_or_default();
    let items: Vec<Value> = rules
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect();
    let pending = crate::automation::list_pending_approvals(&conn).unwrap_or_default();
    let pending_items: Vec<Value> = pending
        .iter()
        .filter_map(|a| serde_json::to_value(a).ok())
        .collect();
    Json(json!({"rules": items, "pending_approvals": pending_items}))
}

/// POST /api/automation/rules
pub async fn create_rule(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let trigger = body
        .get("trigger")
        .and_then(|v| v.as_str())
        .unwrap_or("status_changed");
    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("change_status");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT INTO automation_rules (name, enabled, trigger, action, created_at) VALUES (?1, 1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![name, trigger, action],
    );
    Json(json!({"ok": true}))
}

/// PATCH /api/automation/rules/:id
pub async fn update_rule(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute(
            "UPDATE automation_rules SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        );
    }
    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        let _ = conn.execute(
            "UPDATE automation_rules SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![enabled, id],
        );
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/automation/rules/:id
pub async fn delete_rule(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM automation_rules WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}
