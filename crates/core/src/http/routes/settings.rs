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

/// GET /api/settings/qdrant — reference `ctx.settingsRepo.getQdrant()`:
/// `{ url, enabled }` with defaults `http://127.0.0.1:6333` / `true`.
pub async fn get_qdrant(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let enabled = crate::settings::get_bool(&conn, "qdrant_enabled", true).unwrap_or(true);
    let url = crate::settings::get_string(&conn, "qdrant_url")
        .ok()
        .flatten()
        .unwrap_or_else(|| crate::vectorstore_qdrant::DEFAULT_QDRANT_URL.to_string());
    (
        StatusCode::OK,
        Json(json!({"enabled": enabled, "url": url})),
    )
}

/// PATCH /api/settings/qdrant — reference routes/settings.ts:142-155:
/// validate url (`^https?://\S+$`) + enabled (boolean), persist, reconfigure
/// the adapter, answer `{ok, message}`.
pub async fn update_qdrant(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let url = body.get("url");
    let enabled = body.get("enabled");
    if let Some(u) = url {
        let valid = u.as_str().is_some_and(|s| {
            // ^https?://\S+$
            (s.starts_with("http://") || s.starts_with("https://"))
                && s.len() > "http://".len()
                && !s.contains(char::is_whitespace)
        });
        if !valid {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"ok": false, "message": "Invalid Qdrant URL."})),
            );
        }
    }
    if let Some(e) = enabled {
        if !e.is_boolean() {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"ok": false, "message": "Invalid Qdrant enabled flag."})),
            );
        }
    }
    let (set_url, set_enabled) = {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(u) = url.and_then(|v| v.as_str()) {
            let _ = crate::settings::set_string(&conn, "qdrant_url", u);
        }
        if let Some(e) = enabled.and_then(|v| v.as_bool()) {
            let _ = crate::settings::set_bool(&conn, "qdrant_enabled", e);
        }
        (
            url.and_then(|v| v.as_str()).map(String::from),
            enabled.and_then(|v| v.as_bool()),
        )
    };
    // Reference: ctx.qdrant.reconfigure(body).
    state.qdrant.reconfigure(set_url, set_enabled);
    (
        StatusCode::OK,
        Json(json!({"ok": true, "message": "Qdrant settings saved."})),
    )
}

/// Ensure the `mailbox_business_hours` table exists with the reference's
/// exact DDL (reference migration 008_semantic_docs_sla.ts:37-47).
fn ensure_business_hours_table(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mailbox_business_hours (
            mailbox_local_id INTEGER PRIMARY KEY,
            timezone TEXT NOT NULL DEFAULT 'UTC',
            days TEXT NOT NULL DEFAULT '[1,2,3,4,5]',
            start_minute INTEGER NOT NULL DEFAULT 540,
            end_minute INTEGER NOT NULL DEFAULT 1020,
            first_response_target_min INTEGER,
            resolution_target_min INTEGER,
            updated_at TEXT NOT NULL
        );",
    )
}

/// v1.6.0 audit fix parity: `days` is stored as JSON text; a corrupt row
/// must degrade to null instead of 500ing the route.
fn safe_parse_days(raw: &str) -> Option<Vec<i64>> {
    serde_json::from_str::<Value>(raw).ok().and_then(|v| {
        v.as_array()
            .and_then(|arr| arr.iter().map(|d| d.as_i64()).collect::<Option<Vec<i64>>>())
    })
}

/// GET /api/settings/business-hours — every mailbox in the local mirror with
/// its configured row (reference routes/settings.ts:78-97).
pub async fn get_business_hours(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    if ensure_business_hours_table(&conn).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({"statusCode": 500, "error": "InternalServerError", "message": "Failed to read business hours."}),
            ),
        );
    }
    // Mailbox mirror (reference reads getMailboxes(): id/remote_id/name/email/slug).
    let mailboxes: Vec<(i64, String)> = match conn
        .prepare("SELECT id, name FROM mailboxes ORDER BY name")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map(|rows| rows.flatten().collect())
        }) {
        Ok(v) => v,
        Err(_) => Vec::new(),
    };
    // Configured rows.
    let mut rows: std::collections::HashMap<
        i64,
        (String, String, i64, i64, Option<i64>, Option<i64>),
    > = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min
         FROM mailbox_business_hours",
    ) {
        if let Ok(mapped) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                (
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ),
            ))
        }) {
            for (id, cfg) in mapped.flatten() {
                rows.insert(id, cfg);
            }
        }
    }
    let mailboxes: Vec<Value> = mailboxes
        .into_iter()
        .map(|(id, name)| {
            let row = rows.get(&id);
            json!({
                "mailbox_id": id,
                "name": name,
                "configured": row.is_some(),
                "timezone": row.map(|r| json!(r.0)).unwrap_or(Value::Null),
                "days": row.and_then(|r| safe_parse_days(&r.1)).map(|d| json!(d)).unwrap_or(Value::Null),
                "start_minute": row.map(|r| json!(r.2)).unwrap_or(Value::Null),
                "end_minute": row.map(|r| json!(r.3)).unwrap_or(Value::Null),
                "first_response_target_min": row.and_then(|r| r.4).map(|v| json!(v)).unwrap_or(Value::Null),
                "resolution_target_min": row.and_then(|r| r.5).map(|v| json!(v)).unwrap_or(Value::Null),
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({ "mailboxes": mailboxes })))
}

/// businessHoursSchema (reference shared/schemas.ts:450-460): strict object
/// with timezone 1-64 chars, days = 1-7 unique ints 0-6, start_minute
/// 0-1439, end_minute 1-1440, nullable targets 1-100000, end > start.
fn validate_business_hours(
    body: &Value,
) -> Option<(String, Vec<i64>, i64, i64, Option<i64>, Option<i64>)> {
    let obj = body.as_object()?;
    // strict: no unknown keys
    let allowed = [
        "timezone",
        "days",
        "start_minute",
        "end_minute",
        "first_response_target_min",
        "resolution_target_min",
    ];
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return None;
    }
    let timezone = obj.get("timezone")?.as_str()?;
    if timezone.is_empty() || timezone.len() > 64 {
        return None;
    }
    let days_arr = obj.get("days")?.as_array()?;
    if days_arr.len() < 1 || days_arr.len() > 7 {
        return None;
    }
    let mut days = Vec::new();
    for d in days_arr {
        let n = d.as_i64()?;
        if !(0..=6).contains(&n) {
            return None;
        }
        days.push(n);
    }
    let start = obj.get("start_minute")?.as_i64()?;
    if !(0..=1439).contains(&start) {
        return None;
    }
    let end = obj.get("end_minute")?.as_i64()?;
    if !(1..=1440).contains(&end) {
        return None;
    }
    if end <= start {
        return None;
    }
    let targets = |key: &str| -> Option<Option<i64>> {
        match obj.get(key) {
            None | Some(Value::Null) => Some(None),
            Some(v) => {
                let n = v.as_i64()?;
                if !(1..=100000).contains(&n) {
                    None
                } else {
                    Some(Some(n))
                }
            }
        }
    };
    let first = targets("first_response_target_min")?;
    let resolution = targets("resolution_target_min")?;
    Some((timezone.to_string(), days, start, end, first, resolution))
}

/// IANA timezone validation — the reference uses `Intl.DateTimeFormat`
/// (full ICU). The port uses an embedded IANA database (chrono-tz), the
/// standard Rust equivalent.
fn is_valid_timezone(tz: &str) -> bool {
    tz.parse::<chrono_tz::Tz>().is_ok()
}

/// PUT /api/settings/business-hours/:mailboxId (reference routes/settings.ts:100-130).
pub async fn set_business_hours(
    State(state): State<AppState>,
    axum::extract::Path(mailbox_id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let conn = state.conn_lock();
    let id: i64 = match mailbox_id.parse() {
        Ok(n) if n > 0 => n,
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(
                    json!({"statusCode": 422, "error": "ValidationError", "message": "mailboxId must be a positive integer."}),
                ),
            )
        }
    };
    let mailbox_name: Option<String> = conn
        .query_row("SELECT name FROM mailboxes WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .ok();
    let Some(mailbox_name) = mailbox_name else {
        return (
            StatusCode::NOT_FOUND,
            Json(
                json!({"statusCode": 404, "error": "NotFound", "message": "Mailbox not found in the local mirror."}),
            ),
        );
    };
    let Some((timezone, days, start, end, first, resolution)) = validate_business_hours(&body)
    else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(
                json!({"ok": false, "message": "Invalid business hours: check timezone, days (0-6), and that the end time is after the start time."}),
            ),
        );
    };
    if !is_valid_timezone(&timezone) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(
                json!({"ok": false, "message": "Unknown IANA timezone (e.g. America/New_York, Europe/Berlin, Asia/Kolkata)."}),
            ),
        );
    }
    if ensure_business_hours_table(&conn).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({"statusCode": 500, "error": "InternalServerError", "message": "Failed to save business hours."}),
            ),
        );
    }
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    let days_json = serde_json::to_string(&days).unwrap_or_else(|_| "[1,2,3,4,5]".into());
    let res = conn.execute(
        "INSERT INTO mailbox_business_hours (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(mailbox_local_id) DO UPDATE SET
           timezone=excluded.timezone, days=excluded.days, start_minute=excluded.start_minute,
           end_minute=excluded.end_minute, first_response_target_min=excluded.first_response_target_min,
           resolution_target_min=excluded.resolution_target_min, updated_at=excluded.updated_at",
        rusqlite::params![id, timezone, days_json, start, end, first, resolution, now],
    );
    if res.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(
                json!({"statusCode": 500, "error": "InternalServerError", "message": "Failed to save business hours."}),
            ),
        );
    }
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry::user("business_hours_updated").with_after_state(json!({
            "mailboxId": id, "timezone": timezone, "days": days, "start_minute": start, "end_minute": end,
            "first_response_target_min": first, "resolution_target_min": resolution,
        })),
    );
    (
        StatusCode::OK,
        Json(
            json!({"ok": true, "message": format!("Business hours saved for {mailbox_name}. SLA reports now measure this mailbox in business minutes.")}),
        ),
    )
}

/// DELETE /api/settings/business-hours/:mailboxId (reference routes/settings.ts:132-141).
pub async fn delete_business_hours(
    State(state): State<AppState>,
    axum::extract::Path(mailbox_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let conn = state.conn_lock();
    let id: i64 = match mailbox_id.parse() {
        Ok(n) if n > 0 => n,
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(
                    json!({"statusCode": 422, "error": "ValidationError", "message": "mailboxId must be a positive integer."}),
                ),
            )
        }
    };
    let _ = ensure_business_hours_table(&conn);
    let _ = conn.execute(
        "DELETE FROM mailbox_business_hours WHERE mailbox_local_id = ?1",
        [id],
    );
    (
        StatusCode::OK,
        Json(
            json!({"ok": true, "message": "Business hours cleared - this mailbox falls back to wall-clock minutes in SLA reports."}),
        ),
    )
}

/// GET /api/settings/appearance (reference routes/settings.ts:167-170).
pub async fn appearance(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let tz = crate::settings::get_string(&conn, "display_timezone")
        .ok()
        .flatten()
        .unwrap_or_else(|| "system".to_string());
    (
        StatusCode::OK,
        Json(json!({ "display_timezone": tz, "themes": ["light", "dark"], "default": "light" })),
    )
}

/// POST /api/settings/qdrant/test (reference routes/settings.ts:157-164):
/// `{ ok, connected, url, collections, error, message }`.
pub async fn qdrant_test(State(state): State<AppState>) -> impl IntoResponse {
    // Reference: ctx.qdrant.health() on the live adapter (which reflects any
    // prior reconfigure). The embedded adapter (D2) answers from its own
    // storage state — same shape, same messages.
    let health = state.qdrant.health();
    let message = if health.connected {
        format!(
            "Qdrant reachable at {} ({} collections).",
            health.url,
            health.collections.len()
        )
    } else {
        format!("Qdrant is not reachable at {}. Keyword search (FTS) remains fully functional without it.", health.url)
    };
    (
        StatusCode::OK,
        Json(json!({
            "ok": health.connected,
            "connected": health.connected,
            "url": health.url,
            "collections": health.collections,
            "error": health.error,
            "message": message,
        })),
    )
}
