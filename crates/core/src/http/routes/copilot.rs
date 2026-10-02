//! Copilot routes — mirrors src/server/routes/copilot.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/copilot/sessions
pub async fn list_sessions(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let sessions: Vec<Value> = conn
        .prepare("SELECT id, conversation_id, created_at FROM ai_runs WHERE type = 'copilot' ORDER BY created_at DESC LIMIT 50")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, Option<i64>>(1)?,
                    "created_at": r.get::<_, String>(2)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"sessions": sessions}))
}

/// GET /api/copilot/sessions/:id
pub async fn get_session(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let row = conn.query_row(
        "SELECT id, conversation_id, created_at, result_json FROM ai_runs WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "conversation_id": r.get::<_, Option<i64>>(1)?,
                "created_at": r.get::<_, String>(2)?,
                "messages": r.get::<_, Option<String>>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Session not found"})),
    }
}

/// POST /api/copilot/chat
pub async fn chat(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let conversation_id = body.get("conversationId").and_then(|v| v.as_i64());
    Json(json!({
        "response": "Copilot is not available without a configured AI provider.",
        "message": message,
        "conversationId": conversation_id,
        "tools_used": [],
    }))
}

/// DELETE /api/copilot/sessions/:id
pub async fn delete_session(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute("DELETE FROM ai_runs WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// GET /api/copilot/tools
pub async fn tools(State(state): State<AppState>) -> Json<Value> {
    use crate::catalog::CopilotTool;
    let tools: Vec<Value> = CopilotTool::ALL
        .iter()
        .map(|t| {
            json!({
                "id": t.as_str(),
                "description": t.description(),
            })
        })
        .collect();
    Json(json!({"tools": tools}))
}
