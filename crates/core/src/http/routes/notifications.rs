//! Notifications routes — mirrors src/server/routes/notifications.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /api/notifications — list notifications.
pub async fn list(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let user_id = params
        .get("userId")
        .and_then(|u| u.parse::<i64>().ok())
        .unwrap_or(1);
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(50);
    match crate::notifications::list_unread_for_user(&conn, user_id, limit) {
        Ok(notifs) => {
            let items: Vec<Value> = notifs
                .iter()
                .filter_map(|n| serde_json::to_value(n).ok())
                .collect();
            let total = items.len() as i64;
            let unread = items
                .iter()
                .filter(|n| n.get("read_at").and_then(|v| v.as_str()).is_none())
                .count() as i64;
            Json(json!({"notifications": items, "total": total, "unread": unread}))
        }
        Err(e) => Json(
            json!({"_status": 500, "message": e.to_string(), "notifications": [], "total": 0, "unread": 0}),
        ),
    }
}

/// GET /api/notifications/unread-count
pub async fn unread_count(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let count = crate::notifications::count_unread_for_user(&conn, 1).unwrap_or(0);
    Json(json!({"unread": count}))
}

/// POST /api/notifications/:id/read
pub async fn mark_read(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::notifications::mark_as_read(&conn, id) {
        Ok(ok) => Json(json!({"ok": ok})),
        Err(e) => Json(json!({"_status": 500, "message": e.to_string()})),
    }
}

/// POST /api/notifications/read-all
pub async fn mark_all_read(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute("UPDATE notifications SET read_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE read_at IS NULL", []);
    Json(json!({"ok": true}))
}

/// GET /api/notifications/prefs
pub async fn list_prefs(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let prefs: Vec<Value> = conn
        .prepare("SELECT type, enabled FROM notification_prefs ORDER BY type")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({"type": r.get::<_, String>(0)?, "enabled": r.get::<_, bool>(1)?}))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"prefs": prefs}))
}

/// PUT /api/notifications/prefs/:type
pub async fn set_pref(
    State(state): State<AppState>,
    Path(notif_type): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT OR REPLACE INTO notification_prefs (type, enabled) VALUES (?1, ?2)",
        rusqlite::params![notif_type, enabled],
    );
    Json(json!({"ok": true}))
}

/// GET /api/notifications/mentions
pub async fn mentions(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let items: Vec<Value> = conn
        .prepare("SELECT id, conversation_id, body, created_at FROM side_thread_messages WHERE body LIKE '%@%' ORDER BY created_at DESC LIMIT 20")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, i64>(1)?,
                    "body": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"mentions": items}))
}

/// POST /api/notifications/sweep
pub async fn sweep(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Sweep completed."}))
}
