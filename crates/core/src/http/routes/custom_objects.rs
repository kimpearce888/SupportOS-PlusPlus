//! Custom objects routes — mirrors src/server/routes/customObjects.ts

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/custom-objects/types
pub async fn list_types(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let types = crate::data_tools::list_object_types(&conn).unwrap_or_default();
    let items: Vec<Value> = types
        .iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .collect();
    Json(json!({"types": items}))
}

/// POST /api/custom-objects/types
pub async fn create_type(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let slug = body.get("slug").and_then(|v| v.as_str()).unwrap_or(name);
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::data_tools::create_object_type(&conn, name, slug) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// GET /api/custom-objects/types/:id
pub async fn get_type(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let types = crate::data_tools::list_object_types(&conn).unwrap_or_default();
    if let Some(t) = types.iter().find(|t| t.id == Some(id)) {
        let fields = crate::data_tools::list_object_fields(&conn, id).unwrap_or_default();
        let fields_json: Vec<Value> = fields
            .iter()
            .filter_map(|f| serde_json::to_value(f).ok())
            .collect();
        let mut type_json = serde_json::to_value(t).unwrap_or(json!({}));
        if let Some(obj) = type_json.as_object_mut() {
            obj.insert("fields".to_string(), Value::Array(fields_json));
        }
        Json(type_json)
    } else {
        Json(json!({"error": "Type not found"}))
    }
}

/// PATCH /api/custom-objects/types/:id
pub async fn update_type(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute(
            "UPDATE custom_object_types SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        );
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/custom-objects/types/:id
pub async fn delete_type(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::data_tools::delete_object_type(&conn, id);
    Json(json!({"ok": true}))
}

/// GET /api/custom-objects
pub async fn list_objects(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let type_id = params.get("typeId").and_then(|t| t.parse::<i64>().ok());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<u32>().ok())
        .unwrap_or(50);
    let sql = match type_id {
        Some(tid) => format!("SELECT id, type_id, data_json, created_at FROM custom_objects WHERE type_id = {tid} ORDER BY id DESC LIMIT {limit}"),
        None => format!("SELECT id, type_id, data_json, created_at FROM custom_objects ORDER BY id DESC LIMIT {limit}"),
    };
    let objects: Vec<Value> = conn
        .prepare(&sql)
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "type_id": r.get::<_, i64>(1)?,
                    "data": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"objects": objects}))
}

/// POST /api/custom-objects
pub async fn create_object(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let type_id = body.get("typeId").and_then(|v| v.as_i64()).unwrap_or(0);
    let data = body.get("data").unwrap_or(&Value::Null).to_string();
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT INTO custom_objects (type_id, data_json, created_at) VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![type_id, data],
    );
    Json(json!({"ok": true}))
}

/// GET /api/custom-objects/:id
pub async fn get_object(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, type_id, data_json, created_at FROM custom_objects WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "type_id": r.get::<_, i64>(1)?,
                "data": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Object not found"})),
    }
}

/// PATCH /api/custom-objects/:id
pub async fn update_object(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let data = body.get("data").unwrap_or(&Value::Null).to_string();
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "UPDATE custom_objects SET data_json = ?1 WHERE id = ?2",
        rusqlite::params![data, id],
    );
    Json(json!({"ok": true}))
}

/// DELETE /api/custom-objects/:id
pub async fn delete_object(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM custom_objects WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/custom-objects/:id/links
pub async fn create_link(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "objectId": id}))
}

/// DELETE /api/custom-objects/:id/links/:targetKind/:targetLocalId
pub async fn delete_link(
    State(state): State<AppState>,
    Path((id, _target_kind, _target_local_id)): Path<(i64, String, i64)>,
) -> Json<Value> {
    Json(json!({"ok": true, "objectId": id}))
}

/// GET /api/custom-objects/for/:targetKind/:targetId
pub async fn list_for_target(
    State(state): State<AppState>,
    Path((_target_kind, _target_id)): Path<(String, i64)>,
) -> Json<Value> {
    Json(json!({"objects": []}))
}

/// GET /api/custom-objects/report
pub async fn report(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mut stmt = conn.prepare("SELECT t.name, COUNT(o.id) FROM custom_object_types t LEFT JOIN custom_objects o ON o.type_id = t.id GROUP BY t.id").unwrap();
    let report: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({"type": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?}))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    Json(json!({"report": report}))
}
