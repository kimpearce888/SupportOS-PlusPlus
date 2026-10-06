//! Collaboration routes — mirrors src/server/routes/collaboration.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/conversations/:id/side-threads
///
/// Reference routes/collaboration.ts:31-43: `:id` is the LOCAL conversation
/// id (conversations.id, soft-delete filtered) — the audit B2 fix; the
/// payload is the reference listThreads shape
/// (title/status/message_count/team_name/conversation_number …) — N1.
pub async fn list_side_threads(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> impl IntoResponse {
    if conversation_id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "id must be a positive integer."
            })),
        );
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Check that the conversation exists (local id + not soft-deleted, like
    // the reference's `WHERE id = ? AND deleted_at IS NULL`).
    let exists = conn
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![conversation_id],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Conversation not found."
            })),
        );
    }
    match crate::side_threads::list_side_thread_summaries(&conn, conversation_id) {
        Ok(items) => (StatusCode::OK, Json(json!({ "side_threads": items }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "Internal Server Error",
                "message": e.to_string()
            })),
        ),
    }
}

/// POST /api/conversations/:id/side-threads — create with the FULL
/// reference schema (CL-01 / audit M14): title, anchor team, initial
/// participants and an optional first message. 422 with `path: message`
/// issues on schema violations (missing title, wrong types, unknown
/// participants/teams/creator); 404 for unknown or soft-deleted
/// conversations; 404/422 for invalid thread ids mirror the read routes.
pub async fn create_side_thread(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if conversation_id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "id must be a positive integer."
            })),
        );
    }
    // Parse the full schema first (pure zod-equivalent, no DB).
    let input = match crate::side_threads::parse_create_side_thread_input(&body) {
        Ok(input) => input,
        Err(issues) => {
            let message = issues
                .iter()
                .map(|(path, msg)| format!("{path}: {msg}"))
                .collect::<Vec<_>>()
                .join("; ");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": message
                })),
            );
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // The conversation must exist (LOCAL id + soft-delete filter, like the
    // list route / routes/collaboration.ts:31-43).
    let exists = conn
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![conversation_id],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Conversation not found."
            })),
        );
    }
    // Existence checks for the referenced team / participants / creator
    // (CL-01: 422 on unknown participants/teams).
    let issues = crate::side_threads::check_create_references(&conn, &input);
    if !issues.is_empty() {
        let message = issues
            .iter()
            .map(|(path, msg)| format!("{path}: {msg}"))
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": message
            })),
        );
    }
    match crate::side_threads::create_side_thread_full(
        &conn,
        Some(&state.bus),
        conversation_id,
        &input,
    ) {
        Ok(id) => {
            // Serve the reference-shaped detail alongside the id so callers
            // can render without a follow-up GET (the payload mirrors the
            // GET /api/side-threads/:id contract).
            let detail = crate::side_threads::get_side_thread_detail(&conn, id)
                .ok()
                .flatten();
            (
                StatusCode::OK,
                Json(json!({"ok": true, "id": id, "side_thread": detail})),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "Internal Server Error",
                "message": e.to_string()
            })),
        ),
    }
}

/// GET /api/side-threads/:id
///
/// Reference routes/collaboration.ts:75-87: serves `{ side_thread: … }` —
/// the full detail payload (summary fields + participants + messages with
/// resolved mentions) — 404 for unknown ids, 422 for invalid ones (audit N1:
/// the old handler returned `{ messages: … }` and never 404'd).
pub async fn get_side_thread(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "id must be a positive integer."
            })),
        );
    }
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::side_threads::get_side_thread_detail(&conn, id) {
        Ok(Some(detail)) => (StatusCode::OK, Json(json!({ "side_thread": detail }))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Side thread not found."
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "Internal Server Error",
                "message": e.to_string()
            })),
        ),
    }
}

/// POST /api/side-threads/:id/messages
pub async fn add_message(
    State(state): State<AppState>,
    Path(thread_id): Path<i64>,
    body: Option<Json<Value>>,
) -> Json<Value> {
    let Some(Json(body)) = body else {
        return Json(json!({"ok": false, "error": "Body must be { body: string }."}));
    };
    let text = body.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let author = body.get("authorUserId").and_then(|v| v.as_i64());
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Mention fan-out is immediate (reference sideThreadService.addMessage):
    // the resolved mentions land in side_thread_mentions and every mentioned
    // user / team member gets a Notification Center row + SSE event.
    match crate::side_threads::add_side_thread_message(
        &conn,
        Some(&state.bus),
        thread_id,
        text,
        author,
    ) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// POST /api/side-threads/:id/resolve
pub async fn resolve(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "UPDATE side_threads SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/side-threads/:id/reopen
pub async fn reopen(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "UPDATE side_threads SET resolved_at = NULL WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/side-threads/:id/participants — reference
/// sideThreadRepo.ts:173-186 (CL-04): insert `side_thread_participants`
/// rows with existence checks. Body: `{ userIds: [local user ids] }` (the
/// reference wire name; `user_ids` / `participant_user_ids` are accepted
/// for the port UI's snake_case convention) plus an optional
/// `addedByUserId`. Unknown users or an unknown thread 422/404.
pub async fn add_participants(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    if id <= 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "id must be a positive integer."
            })),
        );
    }
    let Json(body) = body.unwrap_or(Json(Value::Null));
    let user_ids: Vec<i64> = ["userIds", "user_ids", "participant_user_ids"]
        .iter()
        .find_map(|k| {
            body.get(*k)
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect::<Vec<i64>>())
        })
        .unwrap_or_default();
    if user_ids.is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "userIds: Required and must be a non-empty array of user ids."
            })),
        );
    }
    let added_by = ["addedByUserId", "added_by_user_local_id"]
        .iter()
        .find_map(|k| body.get(*k).and_then(|v| v.as_i64()));
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let thread_exists = conn
        .query_row(
            "SELECT 1 FROM side_threads WHERE id = ?1",
            rusqlite::params![id],
            |_| Ok(()),
        )
        .is_ok();
    if !thread_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Side thread not found."
            })),
        );
    }
    match crate::side_threads::add_participants_checked(&conn, id, &user_ids, added_by) {
        Ok(inserted) => (StatusCode::OK, Json(json!({"ok": true, "added": inserted}))),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": e.to_string()
            })),
        ),
    }
}

/// GET /api/mention-directory
pub async fn mention_directory(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let people: Vec<Value> = conn
        .prepare("SELECT id, first_name, last_name FROM users ORDER BY first_name")
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
    Json(json!({"directory": people}))
}
