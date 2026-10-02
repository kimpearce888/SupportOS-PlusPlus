//! Collaboration routes — mirrors src/server/routes/collaboration.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/conversations/:id/side-threads
pub async fn list_side_threads(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let threads = crate::side_threads::list_side_threads_for_conversation(&conn, conversation_id)
        .unwrap_or_default();
    let items: Vec<Value> = threads
        .iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .collect();
    Json(json!({"side_threads": items}))
}

/// POST /api/conversations/:id/side-threads
pub async fn create_side_thread(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let created_by = body.get("createdByUserId").and_then(|v| v.as_i64());
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::side_threads::create_side_thread(&conn, conversation_id, created_by) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// GET /api/side-threads/:id
pub async fn get_side_thread(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let messages = crate::side_threads::list_side_thread_messages(&conn, id).unwrap_or_default();
    let items: Vec<Value> = messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    Json(json!({"messages": items}))
}

/// POST /api/side-threads/:id/messages
pub async fn add_message(
    State(state): State<AppState>,
    Path(thread_id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let text = body.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let author = body.get("authorUserId").and_then(|v| v.as_i64());
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::side_threads::add_side_thread_message(&conn, thread_id, text, author) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// POST /api/side-threads/:id/resolve
pub async fn resolve(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "UPDATE side_threads SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/side-threads/:id/reopen
pub async fn reopen(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute(
        "UPDATE side_threads SET resolved_at = NULL WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/side-threads/:id/participants
pub async fn add_participants(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "threadId": id}))
}

/// GET /api/mention-directory
pub async fn mention_directory(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = conn
        .prepare("SELECT id, first_name, last_name FROM users ORDER BY first_name")
        .unwrap();
    let people: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": format!("{} {}", r.get::<_, String>(1)?, r.get::<_, String>(2)?),
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    Json(json!({"directory": people}))
}
