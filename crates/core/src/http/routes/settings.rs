//! Settings routes — mirrors src/server/routes/settings.ts

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /api/settings — get all settings.
///
/// Reference response shape: a flat object with one key per setting
/// (no nesting). Values are typed (bool, number, string, null).
pub async fn get_settings(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Load all settings from the application_settings table into a HashMap.
    let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM application_settings") {
        if let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
    }
    // Helper: parse a setting value as a typed JSON value.
    fn get_str(
        map: &std::collections::HashMap<String, String>,
        key: &str,
        default: &str,
    ) -> String {
        map.get(key).cloned().unwrap_or_else(|| default.to_string())
    }
    fn get_bool(map: &std::collections::HashMap<String, String>, key: &str, default: bool) -> bool {
        map.get(key)
            .and_then(|v| match v.as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            })
            .unwrap_or(default)
    }
    fn get_i64(map: &std::collections::HashMap<String, String>, key: &str, default: i64) -> i64 {
        map.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
    fn get_opt_i64(
        map: &std::collections::HashMap<String, String>,
        key: &str,
    ) -> Option<serde_json::Value> {
        map.get(key).and_then(|v| {
            if v.is_empty() || v == "null" {
                None
            } else {
                v.parse::<i64>().ok().map(serde_json::Value::from)
            }
        })
    }

    (
        StatusCode::OK,
        Json(json!({
            "sync_interval_minutes": get_i64(&map, "sync_interval_minutes", 5),
            "api_concurrency": get_i64(&map, "api_concurrency", 2),
            "ai_enabled": get_bool(&map, "ai_enabled", true),
            "automatic_analysis_enabled": get_bool(&map, "automatic_analysis_enabled", true),
            "automatic_note_enabled": get_bool(&map, "automatic_note_enabled", false),
            "automatic_draft_enabled": get_bool(&map, "automatic_draft_enabled", false),
            "automation_enabled": get_bool(&map, "automation_enabled", false),
            "automation_write_actions_enabled": get_bool(&map, "automation_write_actions_enabled", false),
            "qdrant_enabled": get_bool(&map, "qdrant_enabled", true),
            "attachment_auto_download": get_bool(&map, "attachment_auto_download", true),
            "automatic_reply_sending": get_bool(&map, "automatic_reply_sending", false),
            "retention_days": get_opt_i64(&map, "retention_days"),
            "backup_interval_hours": get_i64(&map, "backup_interval_hours", 24),
            "log_level": get_str(&map, "log_level", "info"),
            "display_timezone": get_str(&map, "display_timezone", "system"),
            "redaction_enabled": get_bool(&map, "redaction_enabled", true),
            "ai_evaluation_mode": get_bool(&map, "ai_evaluation_mode", false),
            "agent_language": get_str(&map, "agent_language", "en"),
        })),
    )
}

/// PATCH /api/settings — update settings.
pub async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            let val_str = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            let _ = crate::settings::set_string(&conn, key, &val_str);
        }
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// GET /api/settings/lmstudio
pub async fn get_lmstudio(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::ai_center::get_ai_status(&conn) {
        Ok(status) => (
            StatusCode::OK,
            Json(serde_json::to_value(&status).unwrap_or(json!({}))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        ),
    }
}

/// PATCH /api/settings/lmstudio
pub async fn update_lmstudio(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
        let dim = body
            .get("embedding_dim")
            .and_then(|v| v.as_u64())
            .unwrap_or(384) as usize;
        let _ = crate::ai_center::set_embedding_model(&conn, model, dim);
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// POST /api/settings/lmstudio/test
pub async fn test_lmstudio(State(_state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(
            json!({"connected": false, "models": [], "error": "LM Studio test not implemented in HTTP API yet."}),
        ),
    )
}

/// GET /api/settings/qdrant
pub async fn get_qdrant(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let enabled = crate::settings::get_bool(&conn, "qdrant_enabled", false).unwrap_or(false);
    let url = crate::settings::get_string(&conn, "qdrant_url")
        .ok()
        .flatten()
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({"enabled": enabled, "url": url})),
    )
}

/// PATCH /api/settings/qdrant
pub async fn update_qdrant(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        let _ = crate::settings::set_bool(&conn, "qdrant_enabled", enabled);
    }
    if let Some(url) = body.get("url").and_then(|v| v.as_str()) {
        let _ = crate::settings::set_string(&conn, "qdrant_url", url);
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// GET /api/settings/business-hours
pub async fn get_business_hours(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mut stmt = conn
        .prepare("SELECT mailbox_id, config_json FROM sla_configs ORDER BY mailbox_id")
        .unwrap();
    let hours: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({"mailbox_id": r.get::<_, i64>(0)?, "config": r.get::<_, String>(1)?}))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    (StatusCode::OK, Json(json!({"business_hours": hours})))
}

/// PUT /api/settings/business-hours/:mailboxId
pub async fn set_business_hours(
    State(state): State<AppState>,
    axum::extract::Path(mailbox_id): axum::extract::Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let config = body.to_string();
    let _ = conn.execute(
        "INSERT OR REPLACE INTO sla_configs (mailbox_id, config_json) VALUES (?1, ?2)",
        rusqlite::params![mailbox_id, config],
    );
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// DELETE /api/settings/business-hours/:mailboxId
pub async fn delete_business_hours(
    State(state): State<AppState>,
    axum::extract::Path(mailbox_id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM sla_configs WHERE mailbox_id = ?1",
        rusqlite::params![mailbox_id],
    );
    (StatusCode::OK, Json(json!({"ok": true})))
}
