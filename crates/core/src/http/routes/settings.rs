//! Settings routes — mirrors src/server/routes/settings.ts

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/settings — get all settings.
pub async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = conn.prepare("SELECT key, value FROM application_settings ORDER BY key").unwrap();
    let settings: Vec<Value> = stmt.query_map([], |r| {
        Ok(json!({"key": r.get::<_, String>(0)?, "value": r.get::<_, String>(1)?}))
    }).unwrap().filter_map(|r| r.ok()).collect();
    Json(json!({"settings": settings}))
}

/// PATCH /api/settings — update settings.
pub async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            let val_str = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            let _ = crate::settings::set_string(&conn, key, &val_str);
        }
    }
    Json(json!({"ok": true}))
}

/// GET /api/settings/lmstudio
pub async fn get_lmstudio(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::ai_center::get_ai_status(&conn) {
        Ok(status) => Json(serde_json::to_value(&status).unwrap_or(json!({}))),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"message": e.to_string()}))),
    }
}

/// PATCH /api/settings/lmstudio
pub async fn update_lmstudio(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    if let Some(provider) = body.get("provider").and_then(|v| v.as_str()) {
        let kind = crate::ai_center::ProviderKind::parse(provider);
        if let Some(k) = kind {
            let _ = crate::ai_center::set_provider_kind(&conn, k);
        }
    }
    if let Some(model) = body.get("chat_model").and_then(|v| v.as_str()) {
        let _ = crate::ai_center::set_chat_model(&conn, model);
    }
    if let Some(model) = body.get("embedding_model").and_then(|v| v.as_str()) {
        let dim = body.get("embedding_dim").and_then(|v| v.as_u64()).unwrap_or(384) as usize;
        let _ = crate::ai_center::set_embedding_model(&conn, model, dim);
    }
    Json(json!({"ok": true}))
}

/// POST /api/settings/lmstudio/test
pub async fn test_lmstudio(State(_state): State<AppState>) -> impl IntoResponse {
    Json(json!({"connected": false, "models": [], "error": "LM Studio test not implemented in HTTP API yet."}))
}

/// GET /api/settings/qdrant
pub async fn get_qdrant(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let enabled = crate::settings::get_bool(&conn, "qdrant_enabled").unwrap_or(false);
    let url = crate::settings::get_string(&conn, "qdrant_url").unwrap_or_default();
    Json(json!({"enabled": enabled, "url": url}))
}

/// PATCH /api/settings/qdrant
pub async fn update_qdrant(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        let _ = crate::settings::set_bool(&conn, "qdrant_enabled", enabled);
    }
    if let Some(url) = body.get("url").and_then(|v| v.as_str()) {
        let _ = crate::settings::set_string(&conn, "qdrant_url", url);
    }
    Json(json!({"ok": true}))
}

/// GET /api/settings/business-hours
pub async fn get_business_hours(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = conn.prepare("SELECT mailbox_id, config_json FROM sla_configs ORDER BY mailbox_id").unwrap();
    let hours: Vec<Value> = stmt.query_map([], |r| {
        Ok(json!({"mailbox_id": r.get::<_, i64>(0)?, "config": r.get::<_, String>(1)?}))
    }).unwrap().filter_map(|r| r.ok()).collect();
    Json(json!({"business_hours": hours}))
}

/// PUT /api/settings/business-hours/:mailboxId
pub async fn set_business_hours(
    State(state): State<AppState>,
    axum::extract::Path(mailbox_id): axum::extract::Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let config = body.to_string();
    let _ = conn.execute(
        "INSERT OR REPLACE INTO sla_configs (mailbox_id, config_json) VALUES (?1, ?2)",
        rusqlite::params![mailbox_id, config],
    );
    Json(json!({"ok": true}))
}

/// DELETE /api/settings/business-hours/:mailboxId
pub async fn delete_business_hours(
    State(state): State<AppState>,
    axum::extract::Path(mailbox_id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute("DELETE FROM sla_configs WHERE mailbox_id = ?1", rusqlite::params![mailbox_id]);
    Json(json!({"ok": true}))
}
