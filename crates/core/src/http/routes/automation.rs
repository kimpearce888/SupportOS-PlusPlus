//! Automation routes — mirrors src/server/routes/automation.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/automation/rules
///
/// Reference response shape:
/// ```json
/// {
///   "rules": [...],
///   "runs": [...],
///   "risk_tiers": { "read": [...], "non_destructive": [...], "higher_risk": [...], "note": "..." },
///   "automation_enabled": false
/// }
/// ```
pub async fn list_rules(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let rules = crate::automation::list_rules(&conn).unwrap_or_default();
    let items: Vec<Value> = rules
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect();
    let pending = crate::automation::list_pending_approvals(&conn).unwrap_or_default();
    let _pending_items: Vec<Value> = pending
        .iter()
        .filter_map(|a| serde_json::to_value(a).ok())
        .collect();
    let automation_enabled =
        crate::settings::get_bool(&conn, "automation_enabled", false).unwrap_or(false);
    // Risk tiers: classify rules into tiers based on their action.
    // - read: rules that only read state (no writes)
    // - non_destructive: rules that change non-destructive state (priority, tags)
    // - higher_risk: rules that change status, assignee, or send replies
    let read_tiers: Vec<Value> = items
        .iter()
        .filter(|r| {
            let action = r.get("action").and_then(|v| v.as_str()).unwrap_or("");
            action == "tag" || action == "analyze"
        })
        .cloned()
        .collect();
    let non_destructive_tiers: Vec<Value> = items
        .iter()
        .filter(|r| {
            let action = r.get("action").and_then(|v| v.as_str()).unwrap_or("");
            action == "change_priority" || action == "add_tag"
        })
        .cloned()
        .collect();
    let higher_risk_tiers: Vec<Value> = items
        .iter()
        .filter(|r| {
            let action = r.get("action").and_then(|v| v.as_str()).unwrap_or("");
            action == "change_status" || action == "assign" || action == "send_reply"
        })
        .cloned()
        .collect();
    Json(json!({
        "rules": items,
        "runs": [],
        "risk_tiers": {
            "read": read_tiers,
            "non_destructive": non_destructive_tiers,
            "higher_risk": higher_risk_tiers,
            "note": "Rules classified by action risk: read-only < non-destructive (priority/tags) < higher-risk (status/assignee/reply)."
        },
        "automation_enabled": automation_enabled,
    }))
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

/// POST /api/automation/rules/:id/trigger/:conversationId — manual trigger
/// for testing (reference automation.ts:82-88). A missing rule answers
/// `{ ok: false, message: 'Rule not found.' }` with 200 (Fastify default —
/// the reference does not 404 here), and the fired trigger records one run
/// row per matching enabled rule.
pub async fn trigger(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let Some(rule) = crate::automation::load_rule(&conn, id).unwrap_or(None) else {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "message": "Rule not found."})),
        )
            .into_response();
    };
    let runs = crate::automation::fire_trigger(&mut conn, &rule.trigger, conversation_id)
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": format!("Trigger fired ({} runs recorded).", runs.len()),
            "runs": runs,
        })),
    )
        .into_response()
}
