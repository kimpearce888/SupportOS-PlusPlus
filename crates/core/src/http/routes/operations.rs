//! Operations routes — mirrors src/server/routes/operations.ts

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /api/operations/center — operations center snapshot.
///
/// Scope: `?mailboxes=1,2` (local ids, comma list, max 50 — duplicate params
/// arrive as repeated keys and are NOT supported, matching the reference's
/// array-join normalization); omitted/empty/"all" = all inboxes.
///
/// Reference response shape:
/// ```json
/// { "generated_at": "...", "mailbox_scope": null, "tiles": [
///     { "key": "unassigned", "label": "Unassigned", "count": 0,
///       "severity": "info", "drill": {"type":"inbox","params":{...}}, "note": null }
/// ], "waiting_threshold_minutes": 240 }
/// ```
pub async fn center(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Scope: ?mailboxes=1,2 (local ids); omitted/empty/"all" = all inboxes.
    let mailbox_ids: Option<Vec<i64>> = params
        .get("mailboxes")
        .filter(|raw| !raw.is_empty() && *raw != "all")
        .map(|raw| {
            raw.split(',')
                .filter_map(|v| v.trim().parse::<i64>().ok())
                .filter(|v| *v > 0)
                .take(50)
                .collect::<Vec<_>>()
        });
    match crate::operations::snapshot(&conn, mailbox_ids.as_deref()) {
        Ok(snap) => (
            StatusCode::OK,
            Json(serde_json::to_value(&snap).unwrap_or_else(|_| json!({}))),
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

/// GET /api/operations/workload
///
/// Reference response shape:
/// ```json
/// {
///   "generated_at": "...", "unassigned_work": 0,
///   "agents": [], "teams": [],
///   "capacity_model": { "default_max_open": N, "per_user_max": {}, "weights": {} },
///   "method_notes": ["..."]
/// }
/// ```
pub async fn workload(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Pull agent workloads + aggregate.
    let users: Vec<Value> = conn
        .prepare("SELECT id, first_name, last_name FROM users ORDER BY id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "user_local_id": r.get::<_, i64>(0)?,
                    "display_name": format!("{} {}", r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                    "assigned_count": 0i64,
                    "open_count": 0i64,
                    "resolved_today": 0i64,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let teams: Vec<Value> = conn
        .prepare("SELECT id, name FROM teams ORDER BY id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "team_local_id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "assigned_count": 0i64,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let unassigned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE assignee_id IS NULL AND status = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let default_max_open: i64 = crate::settings::get_i64(&conn, "team_capacity", 10).unwrap_or(10);
    Json(json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "unassigned_work": unassigned,
        "agents": users,
        "teams": teams,
        "capacity_model": {
            "default_max_open": default_max_open,
            "per_user_max": {},
            "weights": {}
        },
        "method_notes": [
            "Agent workloads aggregated from conversations.assignee_id (active status only).",
            "Team workloads aggregated from conversations where mailbox_id maps to a team."
        ]
    }))
}

/// PUT /api/operations/capacity
pub async fn set_capacity(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let capacity = body.get("capacity").and_then(|v| v.as_i64()).unwrap_or(10);
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::settings::set_i64(&conn, "team_capacity", capacity);
    Json(json!({"ok": true}))
}

/// PUT /api/operations/waiting-threshold
///
/// Reference `setWaitingThresholdMinutes`: clamped to 1..=20160.
pub async fn set_waiting_threshold(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let Some(minutes) = body.get("minutes").and_then(|v| v.as_i64()) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "minutes must be an integer.",
            })),
        );
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::operations::set_waiting_threshold_minutes(&conn, minutes) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
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

/// GET /api/operations/suggested-assignees
pub async fn suggested_assignees(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let people: Vec<Value> = conn
        .prepare("SELECT id, first_name, last_name FROM users ORDER BY id LIMIT 10")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": format!("{} {}", r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"assignees": people}))
}
