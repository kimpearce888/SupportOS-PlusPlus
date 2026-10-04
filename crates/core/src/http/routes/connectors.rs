//! Connectors routes — mirrors src/server/routes/connectors.ts
//!
//! HTTP configs are SSRF-checked at CREATE/PATCH time (fail fast) and again
//! at every refresh (fail closed). Auth material is NEVER returned — reads
//! always carry the redacted auth object. Refresh runs the snapshot pipeline
//! and reports health honestly (a failed refresh never partially overwrites
//! a previous good snapshot). The test endpoint is honest about what it
//! actually checks (v2.2.1 audit fix): config shape + host resolution, not a
//! full fetch — full reachability is only proven by a refresh.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use super::super::server::AppState;
use crate::connectors;

// ─── response envelopes (reference status shapes) ──────────────────────────

/// Zod parse failure — `reply.code(400)` with the joined issue messages
/// (capped at 300 chars like the reference's `.slice(0, 300)`).
fn bad_request(issues: &[String]) -> Response {
    let joined: String = issues.join("; ").chars().take(300).collect();
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "statusCode": 400,
            "error": "BadRequest",
            "message": joined,
        })),
    )
        .into_response()
}

/// Eager validation refusal (SSRF / jail / missing file) — 422.
fn validation_error(message: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": message,
        })),
    )
        .into_response()
}

/// Repository refusal (duplicate name etc.) — 422 `{ok:false, message}`.
fn unprocessable(message: String) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "ok": false, "message": message })),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Connector not found.",
        })),
    )
        .into_response()
}

/// `Number(request.params.id)` — NaN/garbage lands in `get(NaN) → undefined
/// → 404` in the reference, so non-numeric paths map to a 404 here too.
fn path_id(raw: &str) -> Option<i64> {
    crate::conversation_ops::js_number(raw)
        .filter(|v| v.fract() == 0.0 && *v > 0.0 && *v <= i64::MAX as f64)
        .map(|v| v as i64)
}

/// `clampListParam(value, fallback, min, max)` — helpers.ts:10. Garbage and
/// empty fall back to the default; fractional values truncate.
fn clamp_list_param(raw: Option<&String>, fallback: i64, min: i64, max: i64) -> i64 {
    match raw {
        Some(v) if !v.is_empty() => match crate::conversation_ops::js_number(v) {
            Some(n) if n.is_finite() => (n.trunc() as i64).clamp(min, max),
            _ => fallback,
        },
        _ => fallback,
    }
}

// ─── zod-equivalent validation (shared/workspace.ts) ───────────────────────

/// `nonEmpty(max)` — `z.string().trim().min(1).max(max)`.
fn non_empty(field: &Value, label: &str, max: usize, issues: &mut Vec<String>) -> Option<String> {
    let Value::String(s) = field else {
        issues.push(format!("{label} is required."));
        return None;
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        issues.push(format!("{label} must not be empty."));
    } else if trimmed.chars().count() > max {
        issues.push(format!("{label} must contain at most {max} characters."));
    }
    // The trimmed value is returned even on length violations only when it
    // passed; on issues we return None so the caller never persists junk.
    if trimmed.is_empty() || trimmed.chars().count() > max {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The connectors-jail file name regex `^[^/\\]+[\w\-. ]*$` — net
/// effect: non-empty, and no `/` or `\\` anywhere (no absolute paths,
/// no traversal).
fn is_jail_file_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.contains('\\')
}

/// `keyColumn` — `z.string().trim().max(60).nullable().default(null)`.
fn key_column(config: &Map<String, Value>, issues: &mut Vec<String>) -> Value {
    match config.get("keyColumn") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.chars().count() > 60 {
                issues.push("keyColumn must contain at most 60 characters.".into());
            }
            Value::String(trimmed.to_string())
        }
        Some(_) => {
            issues.push("keyColumn must be a string or null.".into());
            Value::Null
        }
    }
}

/// `connectorConfigSchema` — discriminated union on `kind`. Returns the
/// normalized (stripped + defaulted) config object, or pushes issues.
fn validate_config_schema(body: &Value, issues: &mut Vec<String>) -> Option<Value> {
    let Value::Object(config) = body else {
        issues.push("config is required.".into());
        return None;
    };
    let kind = match config.get("kind") {
        Some(Value::String(k)) => k.as_str(),
        _ => {
            issues.push("config.kind must be one of 'local_json', 'csv', 'sqlite', 'http'.".into());
            return None;
        }
    };
    match kind {
        "local_json" | "csv" | "sqlite" => {
            let mut out = Map::new();
            out.insert("kind".into(), json!(kind));
            let file = match config.get("file") {
                Some(f @ Value::String(_)) => match non_empty(f, "config.file", 200, issues) {
                    Some(file) if !is_jail_file_name(&file) => {
                        issues.push(
                            "File name inside the connectors directory (no absolute paths)".into(),
                        );
                        None
                    }
                    Some(file) => Some(file),
                    None => None,
                },
                _ => {
                    issues.push("config.file is required.".into());
                    None
                }
            };
            file.inspect(|f| {
                out.insert("file".into(), json!(f));
            });
            if kind == "sqlite" {
                let table = match config.get("table") {
                    Some(t @ Value::String(_)) => non_empty(t, "config.table", 60, issues),
                    _ => {
                        issues.push("config.table is required.".into());
                        None
                    }
                };
                match table {
                    Some(t) if !is_table_identifier(&t) => {
                        issues.push("Table name identifier".into());
                    }
                    Some(t) => {
                        out.insert("table".into(), json!(t));
                    }
                    None => {}
                }
            }
            let kc = key_column(config, issues);
            out.insert("keyColumn".into(), kc);
            if issues.is_empty() {
                Some(Value::Object(out))
            } else {
                None
            }
        }
        "http" => {
            let mut out = Map::new();
            out.insert("kind".into(), json!("http"));
            let url = match config.get("url") {
                Some(Value::String(u)) => {
                    let trimmed = u.trim();
                    let len = trimmed.chars().count();
                    if len < 8 {
                        issues.push("config.url must contain at least 8 characters.".into());
                        None
                    } else if len > 600 {
                        issues.push("config.url must contain at most 600 characters.".into());
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                }
                _ => {
                    issues.push("config.url is required.".into());
                    None
                }
            };
            url.inspect(|u| {
                out.insert("url".into(), json!(u));
            });
            let kc = key_column(config, issues);
            out.insert("keyColumn".into(), kc);
            if issues.is_empty() {
                Some(Value::Object(out))
            } else {
                None
            }
        }
        other => {
            issues.push(format!(
                "config.kind must be one of 'local_json', 'csv', 'sqlite', 'http' (got '{other}')."
            ));
            None
        }
    }
}

/// `^[A-Za-z][A-Za-z0-9_]*$` — the whitelisted table identifier shape.
fn is_table_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `connectorAuthSchema` — discriminated union on `mode`. `None` = use the
/// caller's default `{mode:'none'}` (zod's `.default()`).
fn validate_auth_schema(body: &Value, issues: &mut Vec<String>) -> Option<Value> {
    let Value::Object(auth) = body else {
        issues.push("auth must be an object.".into());
        return None;
    };
    let mode = match auth.get("mode") {
        Some(Value::String(m)) => m.as_str(),
        _ => {
            issues.push("auth.mode must be one of 'none', 'header', 'bearer'.".into());
            return None;
        }
    };
    match mode {
        "none" => Some(json!({ "mode": "none" })),
        "header" => {
            let header_name = match auth.get("headerName") {
                Some(h @ Value::String(_)) => match non_empty(h, "auth.headerName", 60, issues) {
                    Some(name) if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                        issues.push("Header names are token identifiers".into());
                        None
                    }
                    Some(name) => Some(name),
                    None => None,
                },
                _ => {
                    issues.push("auth.headerName is required.".into());
                    None
                }
            };
            let header_value = match auth.get("headerValue") {
                Some(v @ Value::String(_)) => match non_empty(v, "auth.headerValue", 500, issues) {
                    Some(value) => Some(value),
                    None => None,
                },
                _ => {
                    issues.push("auth.headerValue is required.".into());
                    None
                }
            };
            match (header_name, header_value) {
                (Some(n), Some(v)) if issues.is_empty() => {
                    Some(json!({ "mode": "header", "headerName": n, "headerValue": v }))
                }
                _ => None,
            }
        }
        "bearer" => {
            let token = match auth.get("token") {
                Some(t @ Value::String(_)) => match non_empty(t, "auth.token", 500, issues) {
                    Some(token) => Some(token),
                    None => None,
                },
                _ => {
                    issues.push("auth.token is required.".into());
                    None
                }
            };
            match token {
                Some(t) if issues.is_empty() => Some(json!({ "mode": "bearer", "token": t })),
                _ => None,
            }
        }
        other => {
            issues.push(format!(
                "auth.mode must be one of 'none', 'header', 'bearer' (got '{other}')."
            ));
            None
        }
    }
}

// ─── routes ────────────────────────────────────────────────────────────────

/// GET /api/connectors
pub async fn list(State(state): State<AppState>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let items: Vec<Value> = connectors::list(&conn)
        .unwrap_or_default()
        .iter()
        .map(|c| connectors::redacted(&conn, c))
        .collect();
    Json(json!({ "connectors": items })).into_response()
}

/// POST /api/connectors — zod create schema, eager SSRF/jail validation,
/// duplicate-name refusal, audit `connector_created`.
pub async fn create(State(state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let Some(Json(body)) = body else {
        return bad_request(&["body is required.".to_string()]);
    };
    let mut issues: Vec<String> = Vec::new();

    // name: nonEmpty(80)
    let name = match body.get("name") {
        Some(n) => non_empty(n, "name", 80, &mut issues),
        None => {
            issues.push("name is required.".into());
            None
        }
    };
    // config: discriminated union on kind
    let config = match body.get("config") {
        Some(c) => validate_config_schema(c, &mut issues),
        None => {
            issues.push("config is required.".into());
            None
        }
    };
    // auth: zod `.default({mode:'none'})` applies ONLY when the key is
    // absent — an explicit null is a validation error, not the default.
    let auth = match body.get("auth") {
        None => json!({ "mode": "none" }),
        Some(a) => validate_auth_schema(a, &mut issues).unwrap_or(json!({ "mode": "none" })),
    };
    // refreshMethod: enum, default 'manual'
    let refresh_method = match body.get("refreshMethod") {
        None => "manual".to_string(),
        Some(Value::String(m)) if m == "manual" || m == "interval" => m.clone(),
        Some(_) => {
            issues.push("refreshMethod must be 'manual' or 'interval'.".into());
            "manual".to_string()
        }
    };
    // refreshSeconds: int 60..=86400, default 3600
    let refresh_seconds = match body.get("refreshSeconds") {
        None => 3600,
        Some(Value::Number(n)) if n.is_u64() => {
            let v = n.as_u64().unwrap_or(3600);
            if (60..=86400).contains(&v) {
                v as i64
            } else {
                issues.push("refreshSeconds must be between 60 and 86400.".into());
                3600
            }
        }
        Some(_) => {
            issues.push("refreshSeconds must be an integer.".into());
            3600
        }
    };
    // allowedAi: bool, default false
    let allowed_ai = match body.get("allowedAi") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            issues.push("allowedAi must be a boolean.".into());
            false
        }
    };
    if !issues.is_empty() {
        return bad_request(&issues);
    }
    let (name, config) = (name.unwrap_or_default(), config.unwrap_or(Value::Null));

    // Eager SSRF/jail validation: refuse unsafe configuration up front.
    let kind = config.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let file = config.get("file").and_then(|v| v.as_str());
    let url = config.get("url").and_then(|v| v.as_str());
    if let Some(problem) = connectors::validate_config(&state.data_dir, kind, file, url).await {
        return validation_error(&problem);
    }

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match connectors::create(
        &conn,
        &name,
        kind,
        &config,
        &auth,
        &refresh_method,
        refresh_seconds,
        allowed_ai,
    ) {
        Ok(connector) => {
            let after = json!({
                "id": connector.get("id"),
                "name": connector.get("name"),
                "kind": connector.get("kind"),
            });
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "connector_created",
                None,
                None,
                Some(&after.to_string()),
                None,
                None,
                false,
            );
            let redacted = connectors::redacted(&conn, &connector);
            Json(json!({ "ok": true, "connector": redacted })).into_response()
        }
        Err(e) => unprocessable(e.to_string()),
    }
}

/// GET /api/connectors/:id
pub async fn get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(id) = path_id(&id) else {
        return not_found();
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match connectors::get(&conn, id).ok().flatten() {
        Some(c) => Json(json!({ "connector": connectors::redacted(&conn, &c) })).into_response(),
        None => not_found(),
    }
}

/// PATCH /api/connectors/:id — partial patch with re-validation of config.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Some(id) = path_id(&id) else {
        return not_found();
    };
    let Some(Json(body)) = body else {
        return bad_request(&["body is required.".to_string()]);
    };
    let mut issues: Vec<String> = Vec::new();
    let mut changes = Map::new();
    let mut fields: Vec<&str> = Vec::new();

    if let Some(n) = body.get("name") {
        if let Some(name) = non_empty(n, "name", 80, &mut issues) {
            changes.insert("name".into(), json!(name));
            fields.push("name");
        }
    }
    if let Some(c) = body.get("config") {
        if let Some(config) = validate_config_schema(c, &mut issues) {
            changes.insert("config".into(), config);
            fields.push("config");
        }
    }
    if let Some(a) = body.get("auth") {
        if let Some(auth) = validate_auth_schema(a, &mut issues) {
            changes.insert("auth".into(), auth);
            fields.push("auth");
        }
    }
    if let Some(rm) = body.get("refreshMethod") {
        match rm.as_str() {
            Some(m) if m == "manual" || m == "interval" => {
                changes.insert("refreshMethod".into(), json!(m));
                fields.push("refreshMethod");
            }
            _ => issues.push("refreshMethod must be 'manual' or 'interval'.".into()),
        }
    }
    if let Some(rs) = body.get("refreshSeconds") {
        match rs.as_u64() {
            Some(v) if (60..=86400).contains(&v) => {
                changes.insert("refreshSeconds".into(), json!(v));
                fields.push("refreshSeconds");
            }
            _ => issues.push("refreshSeconds must be an integer between 60 and 86400.".into()),
        }
    }
    if let Some(ai) = body.get("allowedAi") {
        match ai.as_bool() {
            Some(b) => {
                changes.insert("allowedAi".into(), json!(b));
                fields.push("allowedAi");
            }
            None => issues.push("allowedAi must be a boolean.".into()),
        }
    }
    if let Some(en) = body.get("enabled") {
        match en.as_bool() {
            Some(b) => {
                changes.insert("enabled".into(), json!(b));
                fields.push("enabled");
            }
            None => issues.push("enabled must be a boolean.".into()),
        }
    }
    if !issues.is_empty() {
        return bad_request(&issues);
    }
    let changes = Value::Object(changes);

    // Re-validate config eagerly when it is being replaced.
    if let Some(config) = changes.get("config") {
        let kind = config.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let file = config.get("file").and_then(|v| v.as_str());
        let url = config.get("url").and_then(|v| v.as_str());
        if let Some(problem) = connectors::validate_config(&state.data_dir, kind, file, url).await {
            return validation_error(&problem);
        }
    }

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match connectors::patch(&conn, id, &changes) {
        Ok(Some(connector)) => {
            let after = json!({ "id": id, "fields": fields });
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "connector_updated",
                None,
                None,
                Some(&after.to_string()),
                None,
                None,
                false,
            );
            let redacted = connectors::redacted(&conn, &connector);
            Json(json!({ "ok": true, "connector": redacted })).into_response()
        }
        Ok(None) => not_found(),
        Err(e) => unprocessable(e.to_string()),
    }
}

/// DELETE /api/connectors/:id — removes the connector AND its cached rows.
pub async fn delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(id) = path_id(&id) else {
        return not_found();
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match connectors::delete(&conn, id) {
        Ok(true) => {
            let before = json!({ "id": id });
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "connector_deleted",
                None,
                Some(&before.to_string()),
                None,
                None,
                None,
                false,
            );
            Json(json!({
                "ok": true,
                "message": "Connector and its cached rows deleted."
            }))
            .into_response()
        }
        Ok(false) => not_found(),
        Err(e) => unprocessable(e.to_string()),
    }
}

/// POST /api/connectors/:id/refresh — snapshot refresh; failures are honest
/// 422s carrying the result, never a partial overwrite of a good snapshot.
pub async fn refresh(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(id) = path_id(&id) else {
        return not_found();
    };
    {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        if connectors::get(&conn, id).ok().flatten().is_none() {
            return not_found();
        }
    }
    let result = match connectors::refresh(&state.conn, id, &state.data_dir).await {
        Ok(result) => result,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "statusCode": 500,
                    "error": "InternalServerError",
                    "message": e.to_string()
                })),
            )
                .into_response()
        }
    };
    let result_json = json!({
        "ok": result.ok,
        "rows": result.rows,
        "pruned": result.pruned,
        "schema": result.schema.clone().unwrap_or(Value::Null),
        "error": result.error.clone().map(Value::String).unwrap_or(Value::Null),
    });
    {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let after = json!({
            "id": id,
            "ok": result.ok,
            "rows": result.rows,
            "pruned": result.pruned,
        });
        let _ = crate::jobs::audit(
            &conn,
            "user",
            "connector_refreshed",
            None,
            None,
            Some(&after.to_string()),
            None,
            None,
            false,
        );
    }
    if !result.ok {
        let message = format!(
            "Refresh failed: {}",
            result.error.as_deref().unwrap_or("unknown error")
        );
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "message": message, "result": result_json })),
        )
            .into_response();
    }
    Json(json!({ "ok": true, "result": result_json })).into_response()
}

/// GET /api/connectors/:id/rows — cached rows with SQL-level `q` filtering
/// and reference pagination clamps (pageSize 50/1..200, page 1/1..100000).
pub async fn rows(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = path_id(&id) else {
        return not_found();
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let Some(_connector) = connectors::get(&conn, id).ok().flatten() else {
        return not_found();
    };
    let q: Option<String> = query
        .get("q")
        .map(|q| q.chars().take(120).collect::<String>())
        .filter(|q| !q.is_empty());
    let page_size = clamp_list_param(query.get("pageSize"), 50, 1, 200);
    let page = clamp_list_param(query.get("page"), 1, 1, 100_000);
    let offset = (page - 1) * page_size;
    match connectors::list_rows(&conn, id, q.as_deref(), page_size, offset) {
        Ok((rows, total)) => Json(json!({ "rows": rows, "total": total })).into_response(),
        Err(e) => unprocessable(e.to_string()),
    }
}

/// POST /api/connectors/:id/test — validates config + source host
/// resolution WITHOUT fetching (v2.2.1 honesty fix: the wording claims only
/// what the check performs).
pub async fn test(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(id) = path_id(&id) else {
        return not_found();
    };
    let (kind, file, url) = {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let Some(connector) = connectors::get(&conn, id).ok().flatten() else {
            return not_found();
        };
        let config = connector.get("config").cloned().unwrap_or(Value::Null);
        (
            config
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            config
                .get("file")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            config
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        )
    };
    if let Some(problem) =
        connectors::validate_config(&state.data_dir, &kind, file.as_deref(), url.as_deref()).await
    {
        return Json(json!({ "ok": false, "message": problem })).into_response();
    }
    Json(json!({
        "ok": true,
        "message": "Configuration is valid and the source host resolves. Full reachability is verified on the next refresh."
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn fresh_db() -> Connection {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::search::apply_fts_migration(&conn).unwrap();
        crate::activity::apply_m003(&conn).unwrap();
        crate::ticket_states::apply_m004(&conn).unwrap();
        crate::notifications::apply_m005(&conn).unwrap();
        crate::side_threads::apply_m006(&conn).unwrap();
        crate::automation::apply_m007(&conn).unwrap();
        crate::embeddings::apply_m008(&conn).unwrap();
        crate::ai_center::apply_m009(&conn).unwrap();
        crate::ai_analysis::apply_m010(&conn).unwrap();
        crate::ai_features::apply_m011_to_m013(&conn).unwrap();
        crate::intelligence::apply_m014(&conn).unwrap();
        crate::intelligence_features::apply_m015_to_m019(&conn).unwrap();
        crate::reports::apply_m020_to_m022(&conn).unwrap();
        crate::outreach::apply_m023_to_m025(&conn).unwrap();
        crate::data_tools::apply_m026_to_m027(&conn).unwrap();
        crate::inbox::apply_m028(&conn).unwrap();
        crate::sync_schema::apply_m029(&conn).unwrap();
        crate::conversation_ops::apply_m030(&conn).unwrap();
        crate::outreach::apply_m031(&conn).unwrap();
        crate::ticket_states::apply_m032(&conn).unwrap();
        crate::ai_attributes::apply_m033(&conn).unwrap();
        crate::reports::apply_m034(&conn).unwrap();
        crate::intelligence_features::apply_m035(&conn).unwrap();
        crate::customer_events::apply_m036(&conn).unwrap();
        crate::maintenance::apply_m037(&conn).unwrap();
        crate::connectors::apply_m038(&conn).unwrap();
        crate::mirror_tables::apply_m039(&conn).unwrap();
        conn
    }

    fn make_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState {
            conn: Arc::new(Mutex::new(fresh_db())),
            data_dir: dir.path().to_path_buf(),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        };
        (dir, state)
    }

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    fn write_jail_file(dir: &tempfile::TempDir, name: &str, contents: &str) {
        let jail = dir.path().join("connectors");
        std::fs::create_dir_all(&jail).unwrap();
        std::fs::write(jail.join(name), contents).unwrap();
    }

    fn local_json_body(file: &str) -> Value {
        json!({
            "name": "CRM export",
            "config": { "kind": "local_json", "file": file, "keyColumn": "id" },
            "auth": { "mode": "none" },
            "refreshMethod": "manual",
            "refreshSeconds": 300,
            "allowedAi": false
        })
    }

    #[tokio::test]
    async fn create_validates_body_and_refuses_missing_jail_file() {
        let (_dir, state) = make_state();
        // Missing name → 400 BadRequest (zod).
        let (status, body) = body_json(
            create(
                State(state.clone()),
                Some(Json(
                    json!({ "config": { "kind": "csv", "file": "x.csv" } }),
                )),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "BadRequest");
        assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));

        // File that does not exist in the jail → 422 ValidationError.
        let (status, body) = body_json(
            create(
                State(state.clone()),
                Some(Json(local_json_body("nope.json"))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "ValidationError");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .starts_with("File not found: create \"connectors/nope.json\" first"));
    }

    #[tokio::test]
    async fn create_defaults_and_redaction_round_trip() {
        let (dir, state) = make_state();
        write_jail_file(&dir, "rows.json", r#"[{"id":"a","v":1}]"#);
        // Only the required fields — defaults apply for the rest.
        let (status, body) = body_json(
            create(
                State(state.clone()),
                Some(Json(json!({
                    "name": "Defaults",
                    "config": { "kind": "local_json", "file": "rows.json" }
                }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], json!(true));
        let c = &body["connector"];
        assert_eq!(c["refresh_method"], json!("manual"));
        assert_eq!(c["refresh_seconds"], json!(3600));
        assert_eq!(c["allowed_ai"], json!(false)); // boolean, like the reference
        assert_eq!(c["enabled"], json!(true));
        assert_eq!(c["health"], json!("never"));
        assert_eq!(c["config"]["keyColumn"], json!(null)); // zod default applied
        assert_eq!(c["row_count"], json!(0));
        // Duplicate name → 422 {ok:false, message}.
        let (status, body) = body_json(
            create(
                State(state.clone()),
                Some(Json(json!({
                    "name": "Defaults",
                    "config": { "kind": "local_json", "file": "rows.json" }
                }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["ok"], json!(false));
        assert!(body["message"].as_str().unwrap().contains("already exists"));
        // Bearer auth is redacted on read.
        write_jail_file(&dir, "sec.json", r#"[{"id":"a"}]"#);
        let (status, body) = body_json(
            create(
                State(state.clone()),
                Some(Json(json!({
                    "name": "Sec",
                    "config": { "kind": "local_json", "file": "sec.json" },
                    "auth": { "mode": "bearer", "token": "super-secret-token" }
                }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["connector"]["auth"]["token"], json!("••••••"));
    }

    #[tokio::test]
    async fn refresh_rows_and_test_endpoints_reference_semantics() {
        let (dir, state) = make_state();
        write_jail_file(
            &dir,
            "rows.json",
            r#"[{"id":"v1","env":"production"},{"id":"v2","env":"staging"}]"#,
        );
        let (_, created) = body_json(
            create(
                State(state.clone()),
                Some(Json(local_json_body("rows.json"))),
            )
            .await,
        )
        .await;
        let id = created["connector"]["id"].as_i64().unwrap();
        // NOTE: axum Path<T> derefs to T and has no Clone, so each call
        // site below constructs a fresh Path(id.to_string()).

        // Test endpoint: honest wording, no fetch performed.
        let (status, body) =
            body_json(test(State(state.clone()), Path(id.to_string())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(
            body["message"],
            json!("Configuration is valid and the source host resolves. Full reachability is verified on the next refresh.")
        );

        // Refresh: snapshot lands.
        let (status, body) =
            body_json(refresh(State(state.clone()), Path(id.to_string())).await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["result"]["rows"], json!(2));
        assert_eq!(body["result"]["pruned"], json!(0));
        assert!(body["result"]["schema"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == json!("env")));

        // Rows: q filter is SQL-level (total respects it) + clamp defaults.
        let mut query = HashMap::new();
        query.insert("q".to_string(), "production".to_string());
        let (status, body) =
            body_json(rows(State(state.clone()), Path(id.to_string()), Query(query)).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], json!(1));
        assert_eq!(body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(body["rows"][0]["row_key"], json!("v1"));

        // Second refresh after the source shrinks prunes the vanished key.
        write_jail_file(&dir, "rows.json", r#"[{"id":"v1","env":"production"}]"#);
        let (status, body) =
            body_json(refresh(State(state.clone()), Path(id.to_string())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["rows"], json!(1));
        assert_eq!(body["result"]["pruned"], json!(1));

        // Failed refresh (file deleted) is an honest 422 with the result,
        // and never partially overwrites the previous good snapshot.
        std::fs::remove_file(dir.path().join("connectors").join("rows.json")).unwrap();
        let (status, body) =
            body_json(refresh(State(state.clone()), Path(id.to_string())).await).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["ok"], json!(false));
        // Reference wording: "Refresh failed: ENOENT: no such file or
        // directory, stat '...'" — port: "Refresh failed: config error:
        // file error: No such file or directory (os error 2)". Both honest.
        let message = body["message"].as_str().unwrap();
        assert!(
            message.starts_with("Refresh failed:")
                && message.to_lowercase().contains("no such file"),
            "{message}"
        );
        let mut no_q = HashMap::new();
        let (_, body) =
            body_json(rows(State(state.clone()), Path(id.to_string()), Query(no_q)).await).await;
        assert_eq!(body["total"], json!(1)); // previous snapshot intact
    }

    #[tokio::test]
    async fn unknown_ids_and_bad_paths_are_404() {
        let (_dir, state) = make_state();
        for path in ["999", "abc", "0", "-1"] {
            let (status, _) = body_json(get(State(state.clone()), Path(path.into())).await).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "GET {path}");
        }
        let (status, _) = body_json(refresh(State(state.clone()), Path("999".into())).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = body_json(
            rows(
                State(state.clone()),
                Path("999".into()),
                Query(HashMap::new()),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = body_json(test(State(state.clone()), Path("999".into())).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = body_json(delete(State(state.clone()), Path("999".into())).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn patch_revalidates_config_and_deletes_cascade() {
        let (dir, state) = make_state();
        write_jail_file(&dir, "rows.json", r#"[{"id":"a"}]"#);
        let (_, created) = body_json(
            create(
                State(state.clone()),
                Some(Json(local_json_body("rows.json"))),
            )
            .await,
        )
        .await;
        let id = created["connector"]["id"].as_i64().unwrap();
        // NOTE: axum Path<T> derefs to T and has no Clone, so each call
        // site below constructs a fresh Path(id.to_string()).

        // Patching config to a missing file → 422 ValidationError (eager).
        let (status, body) = body_json(
            update(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({
                    "config": { "kind": "local_json", "file": "missing.json" }
                }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "ValidationError");

        // Valid patch flows through and reports booleans.
        let (status, body) = body_json(
            update(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({ "allowedAi": true, "enabled": false }))),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["connector"]["allowed_ai"], json!(true));
        assert_eq!(body["connector"]["enabled"], json!(false));

        // Delete removes connector + cached rows with the reference message.
        refresh(State(state.clone()), Path(id.to_string())).await;
        let (status, body) =
            body_json(delete(State(state.clone()), Path(id.to_string())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["message"],
            json!("Connector and its cached rows deleted.")
        );
        let (status, _) = body_json(get(State(state.clone()), Path(id.to_string())).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn ssrf_urls_are_refused_at_create_time() {
        let (_dir, state) = make_state();
        // Loopback/private/metadata URLs never reach the repository.
        for url in [
            "http://127.0.0.1:8080/data",
            "http://10.1.2.3/data",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let (status, body) = body_json(
                create(
                    State(state.clone()),
                    Some(Json(json!({
                        "name": "HTTP",
                        "config": { "kind": "http", "url": url }
                    }))),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{url}");
            assert_eq!(body["error"], "ValidationError");
            assert!(
                body["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("URL refused:"),
                "{url}"
            );
        }
    }
}
