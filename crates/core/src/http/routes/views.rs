//! Views routes — mirrors src/server/routes/views.ts

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/inbox-views
pub async fn list_views(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::saved_views::ensure_saved_views_table(&conn);
    let views = crate::inbox::list_saved_views(&conn).unwrap_or_default();
    let items: Vec<Value> = views
        .iter()
        .filter_map(|v| serde_json::to_value(v).ok())
        .collect();
    Json(json!({"views": items}))
}

/// GET /api/inbox-views/:id
pub async fn get_view(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::saved_views::load_view(&conn, id) {
        Ok(view) => Json(serde_json::to_value(&view).unwrap_or(json!({}))),
        Err(_) => Json(json!({"error": "View not found"})),
    }
}

/// POST /api/inbox-views
pub async fn create_view(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let conditions = body.get("conditions").unwrap_or(&Value::Null);
    let mailbox_id = body.get("mailboxId").and_then(|v| v.as_i64());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let view = crate::saved_views::SavedView {
        id: None,
        name: name.to_string(),
        conditions: serde_json::from_value(conditions.clone()).unwrap_or(
            crate::saved_views::ConditionNode::Group {
                op: "and".to_string(),
                children: vec![],
            },
        ),
        mailbox_id,
    };
    match crate::saved_views::save_view(&conn, &view) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// PATCH /api/inbox-views/:id
pub async fn update_view(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute(
            "UPDATE saved_views SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        );
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/inbox-views/:id
pub async fn delete_view(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM saved_views WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/inbox-views/preview
pub async fn preview_view(State(state): State<AppState>, Json(_body): Json<Value>) -> Json<Value> {
    Json(json!({"conversation_ids": []}))
}
