//! Views routes — mirrors src/server/routes/views.ts
//!
//! Saved Inbox View CRUD + dry-run preview (v1.7.0 contract):
//! - definitions are stored as structured JSON trees, compiled at
//!   EVALUATION time by the ViewEngine (a view saved with "today" always
//!   means the day it is opened);
//! - create/update compile-check at SAVE time: a definition that cannot be
//!   evaluated is never persisted (422 `View definition cannot be
//!   evaluated: ...`);
//! - the preview endpoint compiles + counts an UNSAVED definition and never
//!   persists anything.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

use crate::saved_views::{
    parse_create_view_request, parse_update_view_request, parse_view_definition, ViewCompileError,
    ViewEngine,
};

/// The 422 envelope the reference's global Zod handler emits for `.parse()`
/// failures (`app.ts:164-182`): first issue as the message, `issues`
/// capped at 10.
fn zod_envelope(issues: &[(String, String)]) -> (StatusCode, Json<Value>) {
    let Some((first_path, first_message)) = issues.first() else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "Invalid request: request body failed validation.",
                "issues": [],
            })),
        );
    };
    let where_hint = if first_path.is_empty() {
        String::new()
    } else {
        format!(" ({first_path})")
    };
    let issue_list: Vec<Value> = issues
        .iter()
        .take(10)
        .map(|(path, message)| json!({ "path": path, "message": message }))
        .collect();
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": format!("Invalid request{where_hint}: {first_message}"),
            "issues": issue_list,
        })),
    )
}

/// The user's timezone: the stored `display_timezone` setting resolved like
/// the reference's `resolveTimezone(null, settingsRepo.get(...))`.
fn display_timezone(conn: &rusqlite::Connection) -> String {
    let stored = crate::settings::get_string(conn, "display_timezone")
        .ok()
        .flatten()
        .unwrap_or_default();
    crate::saved_views::resolve_timezone(None, Some(stored.as_str()))
}

/// The SLA resolver the route injects (reference views.ts:99: alerts filtered
/// to the requested states, conversation ids extracted).
fn sla_resolver(conn: &rusqlite::Connection) -> crate::saved_views::SlaResolver<'static> {
    // SlaAlerts is computed eagerly (the reference calls slaAlerts() inside
    // the closure; the port's engine is a pure function over the connection,
    // so the ids resolve lazily per query too — but the alerts must be read
    // while the connection guard lives, hence the eager Vec).
    let alerts = crate::sla::sla_alerts(conn).ok();
    Box::new(move |states: &[&str]| {
        alerts
            .as_ref()
            .map(|a| {
                a.alerts
                    .iter()
                    .filter(|alert| states.contains(&alert.state.as_str()))
                    .map(|alert| alert.conversation_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    })
}

/// GET /api/inbox-views
pub async fn list_views(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let views = crate::saved_views::list_views(&conn).unwrap_or_default();
    let items: Vec<Value> = views
        .iter()
        .filter_map(|v| serde_json::to_value(v).ok())
        .collect();
    Json(json!({ "views": items }))
}

/// GET /api/inbox-views/:id
pub async fn get_view(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::saved_views::get_view(&conn, id) {
        Ok(Some(view)) => (
            StatusCode::OK,
            Json(serde_json::to_value(&view).unwrap_or(json!({}))),
        ),
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Saved view not found.",
            })),
        ),
    }
}

/// POST /api/inbox-views — compile-check at save time, then persist.
pub async fn create_view(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let req = match parse_create_view_request(&body) {
        Ok(req) => req,
        Err(issues) => {
            let zod_issues: Vec<(String, String)> = issues
                .iter()
                .map(|i| (i.path.clone(), i.message.clone()))
                .collect();
            return zod_envelope(&zod_issues);
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Compile-check at SAVE time: a definition that cannot be evaluated must
    // never be persisted (the user would only find out when opening it).
    let tz = display_timezone(&conn);
    if let Err(e) = ViewEngine::new(&tz).compile(&req.definition) {
        return compile_rejected(&e);
    }
    match crate::saved_views::create_view(
        &conn,
        &req.name,
        req.description.as_deref(),
        &req.definition,
        req.sort_order.unwrap_or(0),
        req.folder.as_deref(),
    ) {
        Ok(view) => {
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "inbox_view_created",
                None,
                None,
                Some(
                    &serde_json::to_string(&json!({ "view_id": view.id, "name": view.name }))
                        .unwrap_or_default(),
                ),
                None,
                None,
                false,
            );
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "message": format!("View '{}' saved.", view.name),
                    "view": view,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string(),
            })),
        ),
    }
}

/// PATCH /api/inbox-views/:id — same save-time compile check, then patch.
pub async fn update_view(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let req = match parse_update_view_request(&body) {
        Ok(req) => req,
        Err(issues) => {
            let zod_issues: Vec<(String, String)> = issues
                .iter()
                .map(|i| (i.path.clone(), i.message.clone()))
                .collect();
            return zod_envelope(&zod_issues);
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(definition) = req.definition.as_ref() {
        let tz = display_timezone(&conn);
        if let Err(e) = ViewEngine::new(&tz).compile(definition) {
            return compile_rejected(&e);
        }
    }
    match crate::saved_views::update_view(
        &conn,
        id,
        req.name.as_deref(),
        req.description.as_ref().map(|d| d.as_deref()),
        req.definition.as_ref(),
        req.sort_order,
        req.folder.as_ref().map(|f| f.as_deref()),
    ) {
        Ok(Some(view)) => {
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "inbox_view_updated",
                None,
                None,
                Some(
                    &serde_json::to_string(
                        &json!({ "view_id": id, "name": view.name, "version": view.version }),
                    )
                    .unwrap_or_default(),
                ),
                None,
                None,
                false,
            );
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "message": "View updated.",
                    "view": view,
                })),
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Saved view not found.",
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string(),
            })),
        ),
    }
}

/// DELETE /api/inbox-views/:id
pub async fn delete_view(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::saved_views::get_view(&conn, id) {
        Ok(Some(view)) => {
            let _ = crate::saved_views::delete_view(&conn, id);
            let _ = crate::jobs::audit(
                &conn,
                "user",
                "inbox_view_deleted",
                None,
                Some(
                    &serde_json::to_string(&json!({ "view_id": id, "name": view.name }))
                        .unwrap_or_default(),
                ),
                None,
                None,
                None,
                false,
            );
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "message": format!("View '{}' deleted.", view.name),
                })),
            )
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Saved view not found.",
            })),
        ),
    }
}

/// The 422 shape every ViewCompileError maps to (save time and preview).
fn compile_rejected(e: &ViewCompileError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": format!("View definition cannot be evaluated: {}", e.message),
        })),
    )
}

/// POST /api/inbox-views/preview — dry-run: evaluate an UNSAVED definition,
/// return the match count + notes, persist nothing.
pub async fn preview_view(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let raw_tz = body.get("timezone").and_then(|v| v.as_str());
    let raw_definition = body.get("definition").cloned().unwrap_or(Value::Null);
    let definition = match parse_view_definition(&raw_definition) {
        Ok(def) => def,
        Err(issues) => {
            let zod_issues: Vec<(String, String)> = issues
                .iter()
                .map(|i| (i.path.clone(), i.message.clone()))
                .collect();
            return zod_envelope(&zod_issues);
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let stored = crate::settings::get_string(&conn, "display_timezone")
        .ok()
        .flatten()
        .unwrap_or_default();
    let tz = crate::saved_views::resolve_timezone(raw_tz, Some(stored.as_str()));
    let engine = ViewEngine::new(&tz).with_sla_resolver(sla_resolver(&conn));
    let compiled = match engine.compile(&definition) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": e.message,
                })),
            );
        }
    };
    // Count over the same NOT_DELETED base the reference uses (never counts
    // spam-closed or merged-away rows).
    let sql = if compiled.where_sql == "1=1" {
        "SELECT COUNT(*) FROM conversations c WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL"
    } else {
        &format!(
            "SELECT COUNT(*) FROM conversations c WHERE c.deleted_at IS NULL AND c.merged_into_conversation_id IS NULL AND ({})",
            compiled.where_sql
        )
    };
    let count: i64 = match conn.query_row(
        sql,
        rusqlite::params_from_iter(compiled.params.iter()),
        |r| r.get(0),
    ) {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "statusCode": 500,
                    "error": "InternalError",
                    "message": e.to_string(),
                })),
            );
        }
    };
    (
        StatusCode::OK,
        Json(json!({
            "matched": count,
            "notes": compiled.notes,
            "timezone": tz,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
    use std::sync::{Arc, Mutex};

    fn make_state() -> AppState {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        AppState {
            conn: Arc::new(Mutex::new(conn)),
            data_dir: std::path::PathBuf::from("/tmp"),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: std::sync::Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        }
    }

    fn seed_conversation(conn: &rusqlite::Connection, remote: i64, status: &str) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, first_customer_message_at)
             VALUES (?1, ?1, ?2, 1, 2001, '2026-01-02T03:04:05.000Z')",
            rusqlite::params![remote, status],
        )
        .unwrap();
    }

    async fn body_of(r: Response) -> (u16, Value) {
        let status = r.status().as_u16();
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn create_compiles_persists_and_audits() {
        let state = make_state();
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            seed_conversation(&conn, 1001, "active");
            seed_conversation(&conn, 1002, "closed");
        }
        let body = json!({
            "name": "Open tickets",
            "definition": {
                "combinator": "all",
                "conditions": [
                    { "kind": "status", "statuses": ["active"] }
                ]
            },
            "sort_order": 3,
            "folder": "Team"
        });
        let (status, v) = body_of(
            create_view(State(state.clone()), Some(Json(body)))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["message"], json!("View 'Open tickets' saved."));
        assert_eq!(v["view"]["name"], json!("Open tickets"));
        assert_eq!(v["view"]["version"], json!(1));
        assert_eq!(v["view"]["sort_order"], json!(3));
        assert_eq!(v["view"]["folder"], json!("Team"));
        let id = v["view"]["id"].as_i64().unwrap();

        // Scoped guard: the next handler call re-locks the mutex on the same
        // (single-threaded) runtime — the guard must drop before the await.
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            let audits: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE action = 'inbox_view_created' AND actor = 'user'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(audits, 1);
        }

        // GET round trip.
        let (status, v) = body_of(
            get_view(State(state.clone()), Path(id))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(v["name"], json!("Open tickets"));
        assert_eq!(v["definition"]["combinator"], json!("all"));

        // LIST.
        let v = list_views(State(state.clone())).await;
        assert_eq!(v["views"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_rejects_unevaluable_definition_with_422() {
        let state = make_state();
        // Parses fine (value is any bounded string), fails at COMPILE time:
        // the numeric customer_property operator needs a numeric value.
        let body = json!({
            "name": "Bad",
            "definition": {
                "combinator": "all",
                "conditions": [
                    { "kind": "customer_property", "definitionId": 1, "op": "gt", "value": "five" }
                ]
            }
        });
        let (status, v) = body_of(
            create_view(State(state.clone()), Some(Json(body)))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 422, "{v}");
        assert_eq!(v["error"], json!("ValidationError"));
        assert_eq!(
            v["message"],
            json!("View definition cannot be evaluated: customer_property numeric operator requires a numeric value.")
        );
        // Nothing persisted.
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM inbox_views", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn create_rejects_bad_payload_with_zod_envelope() {
        let state = make_state();
        let (status, v) = body_of(
            create_view(State(state.clone()), Some(Json(json!({ "name": "" }))))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 422);
        assert_eq!(v["error"], json!("ValidationError"));
        assert!(v["message"].as_str().unwrap().contains("name"));
        assert!(v["issues"].as_array().unwrap().len() >= 1);
    }

    #[tokio::test]
    async fn update_compiles_patches_and_clears_with_null() {
        let state = make_state();
        let id = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::saved_views::create_view(
                &conn,
                "Old",
                Some("desc"),
                &serde_json::from_value(json!({
                    "combinator": "all",
                    "conditions": [{ "kind": "status", "statuses": ["active"] }]
                }))
                .unwrap(),
                0,
                Some("f"),
            )
            .unwrap()
            .id
        };

        // PATCH a new definition (version bumps).
        let body = json!({
            "name": "New",
            "definition": {
                "combinator": "any",
                "conditions": [{ "kind": "priority", "priorities": ["urgent"] }]
            }
        });
        let (status, v) = body_of(
            update_view(State(state.clone()), Path(id), Some(Json(body)))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["message"], json!("View updated."));
        assert_eq!(v["view"]["name"], json!("New"));
        assert_eq!(v["view"]["version"], json!(2));

        // PATCH null CLEARS the description (3-state).
        let (status, v) = body_of(
            update_view(
                State(state.clone()),
                Path(id),
                Some(Json(json!({"description": null}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["view"]["description"], Value::Null);

        // Unknown id: 404.
        let (status, v) = body_of(
            update_view(
                State(state.clone()),
                Path(999),
                Some(Json(json!({"name": "x"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(v["message"], json!("Saved view not found."));

        // Unevaluable definition: 422, nothing changed.
        let body = json!({
            "definition": {
                "combinator": "all",
                "conditions": [{ "kind": "ai_attribute", "attribute": "urgency", "op": "gt", "value": "nope" }]
            }
        });
        let (status, v) = body_of(
            update_view(State(state.clone()), Path(id), Some(Json(body)))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 422, "{v}");
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let name: String = conn
            .query_row("SELECT name FROM inbox_views WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "New");
    }

    #[tokio::test]
    async fn delete_reports_404_and_audits() {
        let state = make_state();
        let id = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::saved_views::create_view(
                &conn,
                "Gone",
                None,
                &serde_json::from_value(json!({
                    "combinator": "all",
                    "conditions": []
                }))
                .unwrap(),
                0,
                None,
            )
            .unwrap()
            .id
        };
        let (status, _) = body_of(
            delete_view(State(state.clone()), Path(999))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 404);

        let (status, v) = body_of(
            delete_view(State(state.clone()), Path(id))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(v["message"], json!("View 'Gone' deleted."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let audits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'inbox_view_deleted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audits, 1);
    }

    #[tokio::test]
    async fn preview_counts_matches_and_never_persists() {
        let state = make_state();
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            seed_conversation(&conn, 1001, "active");
            seed_conversation(&conn, 1002, "active");
            seed_conversation(&conn, 1003, "closed");
        }
        let body = json!({
            "definition": {
                "combinator": "all",
                "conditions": [{ "kind": "status", "statuses": ["active"] }]
            },
            "timezone": "Europe/Berlin"
        });
        let (status, v) = body_of(
            preview_view(State(state.clone()), Some(Json(body)))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["matched"], json!(2));
        assert_eq!(v["timezone"], json!("Europe/Berlin"));
        assert!(v["notes"].as_array().unwrap().is_empty());

        // Date filters surface honest notes.
        let body = json!({
            "definition": {
                "combinator": "all",
                "conditions": [
                    { "kind": "date_activity", "activityField": "created_at", "mode": "last_30d" }
                ]
            }
        });
        let (status, v) = body_of(
            preview_view(State(state.clone()), Some(Json(body)))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 200, "{v}");
        assert!(v["notes"][0]
            .as_str()
            .unwrap()
            .starts_with("Date filter 'created_at' resolved to Last 30 days"));
        assert_eq!(v["timezone"], json!("UTC"));

        // Nothing persisted by preview.
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM inbox_views", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn preview_422s_on_bad_definition_and_compile_errors() {
        let state = make_state();
        // Unknown kind -> the Zod envelope.
        let (status, v) = body_of(
            preview_view(State(state.clone()), Some(Json(json!({ "definition": { "combinator": "all", "conditions": [{ "kind": "nope" }] } }))))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(status, 422, "{v}");
        assert_eq!(v["error"], json!("ValidationError"));

        // Parses, fails at compile: exact_date needs a valid from date.
        let (status, v) = body_of(
            preview_view(
                State(state.clone()),
                Some(Json(json!({
                    "definition": {
                        "combinator": "all",
                        "conditions": [
                            { "kind": "date_activity", "activityField": "created_at", "mode": "exact_date", "from": "not-a-date" }
                        ]
                    }
                }))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(status, 422, "{v}");
        assert_eq!(
            v["message"],
            json!("Date filter 'exact_date' requires valid from/to dates (YYYY-MM-DD).")
        );
    }
}
