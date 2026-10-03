//! Conversation routes — mirrors src/server/routes/conversations.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// Eval-mode rejection in the tuple shape these handlers return (the shared
/// `conversation_ops::rejected` builds a `Response`, which cannot mix with
/// `impl IntoResponse` tuple arms).
fn eval_rejected(msg: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "ok": false, "message": msg })),
    )
}

/// GET /api/conversations — list conversations with filters.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());

    let mut filters = crate::inbox::InboxFilters::default();
    if let Some(status) = params.get("view") {
        filters.status = Some(status.clone());
    }
    if let Some(priority) = params.get("priority") {
        filters.priority = Some(priority.clone());
    }
    if let Some(query) = params.get("q") {
        filters.query = Some(query.clone());
    }
    if let Some(limit) = params.get("pageSize") {
        filters.limit = limit.parse().ok();
    }

    match crate::inbox::list_conversations(&conn, &filters) {
        Ok((items, total)) => {
            let items_json: Vec<Value> = items
                .iter()
                .filter_map(|i| serde_json::to_value(i).ok())
                .collect();
            (
                StatusCode::OK,
                Json(json!({
                    "conversations": items_json,
                    "total": total,
                    "page": 1,
                    "page_size": 50,
                    "view": params.get("view").cloned().unwrap_or_default(),
                    "notes": [],
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// GET /api/conversations/:id — conversation detail with threads.
pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::inbox::get_conversation(&conn, id) {
        Ok(Some(detail)) => match serde_json::to_value(&detail) {
            Ok(mut v) => {
                // Sanitize untrusted thread HTML before it leaves the server
                // (reference routes/conversations.ts:180 — sanitizeThreadHtml
                // on body_html). The port's thread body carries the same
                // untrusted email HTML.
                if let Some(threads) = v["thread"].as_array_mut() {
                    for t in threads {
                        if let Some(body) = t.get("body").and_then(|b| b.as_str()) {
                            t["body"] = serde_json::Value::String(
                                crate::security::sanitize_thread_html(body),
                            );
                        }
                    }
                }
                (StatusCode::OK, Json(v))
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()}),
                ),
            ),
        },
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(
                json!({"statusCode": 404, "error": "NotFound", "message": "Conversation not found locally."}),
            ),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// POST /api/conversations/:id/reply
pub async fn reply(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let body_text = body.get("body").and_then(|v| v.as_str()).unwrap_or("");
    if body_text.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(
                json!({"statusCode": 422, "error": "ValidationError", "message": "Reply body cannot be empty."}),
            ),
        );
    }
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant (operations.ts sendReply): eval mode blocks EVERY
    // remote write — replies included.
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    let result = crate::inbox::reply_to_conversation(
        &mut conn,
        id,
        body_text.to_string(),
        "user".to_string(),
        None,
    );
    drop(conn);
    match result {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            if ok {
                crate::http::event_bus::notify_conversation_updated(
                    &state.bus,
                    &crate::events::ConversationUpdatedEvent {
                        conversation_id: Some(id),
                        conversation_number: None,
                        mailbox_id: None,
                        subject: None,
                        reason: "sync".into(),
                        at: chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string(),
                    },
                );
            }
            (
                StatusCode::OK,
                Json(json!({"ok": ok, "message": "Reply sent."})),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// POST /api/conversations/:id/note
pub async fn note(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let body_text = body.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant (operations.ts addNote): eval mode blocks notes too.
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    let result = crate::inbox::add_note(
        &mut conn,
        id,
        body_text.to_string(),
        "user".to_string(),
        None,
    );
    drop(conn);
    match result {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            if ok {
                crate::http::event_bus::notify_conversation_updated(
                    &state.bus,
                    &crate::events::ConversationUpdatedEvent {
                        conversation_id: Some(id),
                        conversation_number: None,
                        mailbox_id: None,
                        subject: None,
                        reason: "sync".into(),
                        at: chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string(),
                    },
                );
            }
            (StatusCode::OK, Json(json!({"ok": ok})))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// POST /api/conversations/:id/status
pub async fn status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let new_status = body
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("active");
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant (operations.ts changeStatus): eval mode blocks status
    // changes.
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    let result = crate::inbox::change_status(
        &mut conn,
        id,
        new_status.to_string(),
        "user".to_string(),
        None,
    );
    drop(conn);
    match result {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            if ok {
                crate::http::event_bus::notify_conversation_updated(
                    &state.bus,
                    &crate::events::ConversationUpdatedEvent {
                        conversation_id: Some(id),
                        conversation_number: None,
                        mailbox_id: None,
                        subject: None,
                        reason: "sync".into(),
                        at: chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string(),
                    },
                );
            }
            (StatusCode::OK, Json(json!({"ok": ok})))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// POST /api/conversations/:id/assign
pub async fn assign(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let assignee = body.get("assigneeLocalId").and_then(|v| v.as_i64());
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant (operations.ts assignTo): eval mode blocks assignments.
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    let result = crate::inbox::assign(&mut conn, id, assignee, "user".to_string(), None);
    drop(conn);
    match result {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            if ok {
                crate::http::event_bus::notify_conversation_updated(
                    &state.bus,
                    &crate::events::ConversationUpdatedEvent {
                        conversation_id: Some(id),
                        conversation_number: None,
                        mailbox_id: None,
                        subject: None,
                        reason: "sync".into(),
                        at: chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string(),
                    },
                );
            }
            (StatusCode::OK, Json(json!({"ok": ok})))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// POST /api/conversations/:id/priority
pub async fn priority(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let priority = body
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("normal");
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant: eval mode blocks every remote write, priority
    // changes included.
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    use crate::ticket_ops::{execute, TicketOperation};
    let op = TicketOperation::SetPriority {
        conversation_remote_id: id,
        new_priority: crate::ticket_states::TicketPriority::parse(priority)
            .unwrap_or(crate::ticket_states::TicketPriority::Normal),
        actor_type: "user".to_string(),
        actor_id: None,
    };
    match execute(&mut conn, &op) {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            drop(conn);
            if ok {
                crate::http::event_bus::notify_conversation_updated(
                    &state.bus,
                    &crate::events::ConversationUpdatedEvent {
                        conversation_id: Some(id),
                        conversation_number: None,
                        mailbox_id: None,
                        subject: None,
                        reason: "sync".into(),
                        at: chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string(),
                    },
                );
            }
            (StatusCode::OK, Json(json!({"ok": ok})))
        }
        Err(e) => {
            drop(conn);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()}),
                ),
            )
        }
    }
}

/// POST /api/conversations/:id/subject — edit subject.
pub async fn subject(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let new_subject = body.get("subject").and_then(|v| v.as_str()).unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant: eval mode blocks subject edits (remote write).
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    let result = conn.execute(
        "UPDATE conversations SET subject = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE remote_id = ?2",
        rusqlite::params![new_subject, id],
    );
    let updated = result.as_ref().map(|rows| *rows > 0).unwrap_or(false);
    drop(conn);
    if updated {
        crate::http::event_bus::notify_conversation_updated(
            &state.bus,
            &crate::events::ConversationUpdatedEvent {
                conversation_id: Some(id),
                conversation_number: None,
                mailbox_id: None,
                subject: None,
                reason: "sync".into(),
                at: chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string(),
            },
        );
    }
    match result {
        Ok(rows) => (StatusCode::OK, Json(json!({"ok": rows > 0}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()})),
        ),
    }
}

/// POST /api/conversations/:id/state — set custom ticket state.
pub async fn set_state(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let new_state = body.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let mut conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Safety invariant: eval mode blocks custom-state changes (status
    // family).
    if let Some(msg) = eval_mode_blocked(&conn) {
        drop(conn);
        return eval_rejected(msg);
    }
    use crate::ticket_ops::{execute, TicketOperation};
    let op = TicketOperation::SetTicketState {
        conversation_remote_id: id,
        new_state: new_state.to_string(),
        actor_type: "user".to_string(),
        actor_id: None,
    };
    match execute(&mut conn, &op) {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            drop(conn);
            if ok {
                crate::http::event_bus::notify_conversation_updated(
                    &state.bus,
                    &crate::events::ConversationUpdatedEvent {
                        conversation_id: Some(id),
                        conversation_number: None,
                        mailbox_id: None,
                        subject: None,
                        reason: "sync".into(),
                        at: chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string(),
                    },
                );
            }
            (StatusCode::OK, Json(json!({"ok": ok})))
        }
        Err(e) => {
            drop(conn);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()}),
                ),
            )
        }
    }
}

/// GET /api/conversations/:id/events — activity events for a conversation.
pub async fn events(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mut stmt = match conn.prepare(
        "SELECT id, conversation_id, event_type, actor_type, actor_id, occurred_at
         FROM activity_events WHERE conversation_id = ?1 ORDER BY occurred_at ASC",
    ) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()}),
                ),
            );
        }
    };
    let events: Vec<Value> = stmt
        .query_map(rusqlite::params![id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "conversation_id": r.get::<_, i64>(1)?,
                "event_type": r.get::<_, String>(2)?,
                "actor_type": r.get::<_, String>(3)?,
                "actor_id": r.get::<_, Option<i64>>(4)?,
                "occurred_at": r.get::<_, String>(5)?,
            }))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    (StatusCode::OK, Json(json!({"events": events})))
}

/// POST /api/conversations/activity/rebuild — rebuild activity events.
pub async fn activity_rebuild(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({"ok": true, "message": "Activity rebuild queued."})),
    )
}

/// GET /api/ticket-states — list custom ticket states.
pub async fn list_ticket_states(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let states: Vec<Value> = crate::ticket_states::list_states(&conn)
        .iter()
        .filter_map(|s| serde_json::to_value(s).ok())
        .collect();
    let bottlenecks = crate::ticket_states::state_bottlenecks(&conn);
    (
        StatusCode::OK,
        Json(json!({ "states": states, "bottlenecks": bottlenecks })),
    )
}

/// POST /api/ticket-states — create a custom state definition (reference
/// conversations.ts:303-312 + ticketStateRepo.createState).
pub async fn create_ticket_state(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    // createStateRequestSchema: name 1..80 required; key optional
    // /^[a-z0-9-]+$/ <=60; color #rrggbb nullable; sort_order 0..=9999;
    // is_resolved bool.
    let Some(name) = body.get("name").and_then(|v| v.as_str()) else {
        return crate::conversation_ops::zod_422("name", "Required");
    };
    if name.is_empty() {
        return crate::conversation_ops::zod_422(
            "name",
            "String must contain at least 1 character(s)",
        );
    }
    if name.len() > 80 {
        return crate::conversation_ops::zod_422(
            "name",
            "String must contain at most 80 character(s)",
        );
    }
    let key = body.get("key").and_then(|v| v.as_str());
    if let Some(k) = key {
        if k.is_empty()
            || k.len() > 60
            || !k
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return crate::conversation_ops::zod_422(
                "key",
                "key must be lowercase letters, digits and dashes",
            );
        }
    }
    let color = match body.get("color") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::String(c)) => {
            let ok =
                c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit());
            if !ok {
                return crate::conversation_ops::zod_422("color", "color must be a #rrggbb hex");
            }
            Some(c.as_str())
        }
        Some(_) => {
            return crate::conversation_ops::zod_422(
                "color",
                "Expected string, received non-string",
            )
        }
    };
    let sort_order = match body.get("sort_order") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => match v.as_i64() {
            Some(n) if (0..=9999).contains(&n) => Some(n),
            Some(_) => {
                return crate::conversation_ops::zod_422(
                    "sort_order",
                    "Number must be greater than or equal to 0",
                )
            }
            None => {
                return crate::conversation_ops::zod_422(
                    "sort_order",
                    "Expected number, received non-number",
                )
            }
        },
    };
    let is_resolved = body.get("is_resolved").and_then(|v| v.as_bool());
    let conn = state.conn_lock();
    match crate::ticket_states::create_state(&conn, name, key, color, sort_order, is_resolved) {
        Ok(st) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "message": format!("State '{}' created.", st.name),
                "state": st,
            })),
        )
            .into_response(),
        Err(msg) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "message": msg })),
        )
            .into_response(),
    }
}

/// PATCH /api/ticket-states/:id — update a state definition.
pub async fn update_ticket_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Ok(id) = id.parse::<i64>() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "message": "State not found." })),
        )
            .into_response();
    };
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        if name.is_empty() {
            return crate::conversation_ops::zod_422(
                "name",
                "String must contain at least 1 character(s)",
            );
        }
        if name.len() > 80 {
            return crate::conversation_ops::zod_422(
                "name",
                "String must contain at most 80 character(s)",
            );
        }
    }
    if let Some(color) = body.get("color").and_then(|v| v.as_str()) {
        let ok = color.len() == 7
            && color.starts_with('#')
            && color[1..].chars().all(|h| h.is_ascii_hexdigit());
        if !ok {
            return crate::conversation_ops::zod_422("color", "color must be a #rrggbb hex");
        }
    }
    let conn = state.conn_lock();
    match crate::ticket_states::update_state(&conn, id, &body) {
        Ok(Some(st)) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "message": "State updated.", "state": st })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "message": "State not found." })),
        )
            .into_response(),
        Err(msg) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "message": msg })),
        )
            .into_response(),
    }
}

/// DELETE /api/ticket-states/:id — delete a custom state.
pub async fn delete_ticket_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Ok(id) = id.parse::<i64>() else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "ok": false, "message": "State not found." })),
        )
            .into_response();
    };
    let conn = state.conn_lock();
    let (ok, message) = crate::ticket_states::delete_state(&conn, id);
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    (code, Json(json!({ "ok": ok, "message": message }))).into_response()
}

/// GET /api/mailboxes — list all mailboxes (reference data for the UI).
pub async fn list_mailboxes(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mailboxes: Vec<Value> = conn
        .prepare("SELECT id, remote_id, name, email FROM mailboxes ORDER BY id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "remote_id": r.get::<_, i64>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "email": r.get::<_, Option<String>>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    (StatusCode::OK, Json(mailboxes))
}

/// GET /api/tags — list all tags (reference data for the UI).
pub async fn list_tags(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    // Reference getTags: id, remote_id, name, slug, color, ticket_count
    // straight from the synced tags table, ordered by name.
    let tags: Vec<Value> = conn
        .prepare(
            "SELECT id, remote_id, name, slug, color, COALESCE(ticket_count, 0)
               FROM tags ORDER BY name",
        )
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "remote_id": r.get::<_, Option<i64>>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "slug": r.get::<_, Option<String>>(3)?,
                    "color": r.get::<_, Option<String>>(4)?,
                    "ticket_count": r.get::<_, i64>(5)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    (StatusCode::OK, Json(tags))
}

/// GET /api/users — list all users + system users (reference data for the UI).
pub async fn list_users(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let users: Vec<Value> = conn
        .prepare("SELECT id, remote_id, first_name, last_name, email FROM users ORDER BY id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                let first: String = r.get::<_, String>(2).unwrap_or_default();
                let last: String = r.get::<_, String>(3).unwrap_or_default();
                let display_name = format!("{first} {last}").trim().to_string();
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "remote_id": r.get::<_, i64>(1)?,
                    "display_name": display_name,
                    "email": r.get::<_, Option<String>>(4)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({"users": users, "system_users": []})),
    )
}

/// GET /api/teams — list all teams (reference data for the UI).
pub async fn list_teams(State(state): State<AppState>) -> impl IntoResponse {
    // Reference returns a BARE ARRAY of {id, remote_id, name, member_count}
    // (member_count from the user mirror's team membership).
    let conn = state.conn_lock();
    let teams: Vec<Value> = conn
        .prepare(
            "SELECT t.id, t.remote_id, t.name,
                    (SELECT COUNT(*) FROM users u WHERE u.team_id = t.id) AS member_count
             FROM teams t ORDER BY t.id",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "remote_id": r.get::<_, Option<i64>>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "member_count": r.get::<_, i64>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    (StatusCode::OK, Json(teams))
}

/// GET /api/saved-replies — list saved reply templates; `?q=` searches
/// (reference: `?q` -> searchSavedReplies, else getSavedReplies).
pub async fn list_saved_replies(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn_lock();
    let q = params.get("q").map(String::as_str);
    let replies = crate::mirror_readouts::saved_replies(&conn, q).unwrap_or_default();
    (StatusCode::OK, Json(json!({ "saved_replies": replies })))
}

/// GET /api/inbox-fields — custom field definitions with options.
pub async fn inbox_fields(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let fields = crate::mirror_readouts::inbox_fields(&conn).unwrap_or_default();
    (StatusCode::OK, Json(fields))
}

/// GET /api/workflows — mailbox workflows mirror.
pub async fn workflows(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let list = crate::mirror_readouts::workflows(&conn).unwrap_or_default();
    (StatusCode::OK, Json(list))
}

/// GET /api/users/statuses — user presence statuses.
pub async fn user_statuses(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let list = crate::mirror_readouts::user_statuses(&conn).unwrap_or_default();
    (StatusCode::OK, Json(list))
}

/// GET /api/webhook-configs — registered Help Scout webhook configurations.
pub async fn webhook_configs(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let list = crate::mirror_readouts::webhook_configs(&conn).unwrap_or_default();
    (StatusCode::OK, Json(list))
}

// ---------------------------------------------------------------------------
// Conversation write operations (reference conversations.ts:385-477 — the
// operations.ts write pipeline: validate → authorize → job → write →
// confirm → persist → audit). Handlers use JS Number() semantics for :id so
// non-numeric ids reproduce the reference's Zod 422s instead of Axum 400s.
// ---------------------------------------------------------------------------

use crate::conversation_ops::{
    conv_by_local_id, eval_mode_blocked, js_number, op_bulk_action, op_delete_schedule,
    op_move_to_inbox, op_publish_schedule, op_run_workflow, op_schedule_reply, op_snooze,
    op_unsnooze, op_update_custom_fields, op_update_tags, rejected, zod_422, zod_422_multi,
    zod_bool_msg, zod_enum_message, zod_int_msg, zod_string_msg,
};
use axum::response::Response;

/// Parse `:id` with JS `Number()` + `z.number().int()` semantics.
fn path_id(raw: &str) -> Result<i64, Response> {
    match js_number(raw) {
        Some(f)
            if f.is_finite()
                && f.fract() == 0.0
                && f >= i64::MIN as f64
                && f <= i64::MAX as f64 =>
        {
            Ok(f as i64)
        }
        Some(_) => Err(zod_422(
            "conversationId",
            "Expected integer, received float",
        )),
        None => Err(zod_422("conversationId", "Expected number, received nan")),
    }
}

/// POST /api/conversations/:id/move — move to another inbox.
pub async fn move_to_inbox(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mailbox_id = match zod_int_msg(&body, "mailboxId") {
        Ok(v) => v,
        Err(m) => return zod_422("mailboxId", &m),
    };
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        if conv_by_local_id(&conn, conversation_id).is_none() {
            return rejected("Conversation not found locally.");
        }
    }
    // Real mode: PATCH the remote first (reference realProvider.updateConversation).
    if let Some(real) = state.real.clone() {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = real
            .request(
                &format!("/v2/conversations/{remote_id}"),
                "PATCH",
                Some(json!({ "op": "move", "path": "/mailboxId", "value": mailbox_id })),
            )
            .await
        {
            return rejected(&format!("Conversation was NOT moved. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_move_to_inbox(&conn, conversation_id, mailbox_id)
}

/// POST /api/conversations/:id/tags — merge-semantics tag update.
pub async fn update_tags_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    // add/remove default [], set nullish
    let mut change = json!({});
    for key in ["add", "remove"] {
        match body.get(key) {
            None | Some(Value::Null) => {
                change[key] = json!([]);
            }
            Some(Value::Array(a)) => {
                for v in a {
                    if !v.is_string() {
                        return zod_422(
                            key,
                            &format!(
                                "Expected string, received {}",
                                if v.is_number() {
                                    "number"
                                } else if v.is_boolean() {
                                    "boolean"
                                } else {
                                    "object"
                                }
                            ),
                        );
                    }
                }
                change[key] = json!(a);
            }
            Some(_) => return zod_422(key, "Expected array, received non-array"),
        }
    }
    match body.get("set") {
        Some(Value::Null) | None => {}
        Some(Value::Array(a)) => {
            change["set"] = json!(a);
        }
        Some(_) => return zod_422("set", "Expected array, received non-array"),
    }
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        if conv_by_local_id(&conn, conversation_id).is_none() {
            return rejected("Conversation not found locally.");
        }
    }
    // Real mode: PUT the complete desired state (fresh-read happens remotely).
    if let Some(real) = state.real.clone() {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Ok(remote) = real
            .request(&format!("/v2/conversations/{remote_id}"), "GET", None)
            .await
        {
            let current: Vec<String> = remote["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t["name"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let desired = crate::conversation_ops::merge_tags(&current, &change);
            if let Err(e) = real
                .request(
                    &format!("/v2/conversations/{remote_id}/tags"),
                    "PUT",
                    Some(json!({ "tags": desired })),
                )
                .await
            {
                return rejected(&format!("Tags were NOT changed. {e}"));
            }
        }
    }
    let conn = state.conn_lock();
    op_update_tags(&conn, conversation_id, &change)
}

/// POST /api/conversations/:id/fields — custom field update.
pub async fn update_fields_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(fields) = body.get("fields").and_then(|f| f.as_array()) else {
        return zod_422("fields", "Required");
    };
    let mut parsed: Vec<(i64, Option<String>)> = Vec::new();
    for f in fields {
        let Some(fid) = f.get("id").and_then(|v| v.as_i64()) else {
            return zod_422("fields", "Expected object with integer id");
        };
        // value: z.string().nullish() — string, null or absent.
        let value: Option<String> = match f.get("value") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            Some(_) => {
                return zod_422("value", "Expected string, received non-string");
            }
        };
        parsed.push((fid, value));
    }
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        if conv_by_local_id(&conn, conversation_id).is_none() {
            return rejected("Conversation not found locally.");
        }
    }
    if let Some(real) = state.real.clone() {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        let payload = json!({ "fields": parsed.iter().map(|(id, v)| json!({
            "id": id, "value": v.clone().unwrap_or_default()
        })).collect::<Vec<_>>() });
        if let Err(e) = real
            .request(
                &format!("/v2/conversations/{remote_id}/fields"),
                "PUT",
                Some(payload),
            )
            .await
        {
            return rejected(&format!("Fields were NOT changed. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_update_custom_fields(&conn, conversation_id, &parsed)
}

/// POST /api/conversations/:id/snooze
pub async fn snooze_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Ok(snoozed_until) = zod_string_msg(&body, "snoozedUntil") else {
        return zod_422("snoozedUntil", "Required");
    };
    // unsnoozeOnCustomerReply defaults to true.
    let _unsnooze = match body.get("unsnoozeOnCustomerReply") {
        None | Some(Value::Null) => true,
        _ => match zod_bool_msg(&body, "unsnoozeOnCustomerReply") {
            Ok(b) => b,
            Err(m) => return zod_422("unsnoozeOnCustomerReply", &m),
        },
    };
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        if conv_by_local_id(&conn, conversation_id).is_none() {
            return rejected("Conversation not found locally.");
        }
    }
    if let Some(real) = state.real.clone() {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = real
            .request(
                &format!("/v2/conversations/{remote_id}/snooze"),
                "PUT",
                Some(
                    json!({ "snoozedUntil": snoozed_until, "unsnoozeOnCustomerReply": _unsnooze }),
                ),
            )
            .await
        {
            return rejected(&format!("Snooze was NOT applied. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_snooze(&conn, conversation_id, &snoozed_until)
}

/// DELETE /api/conversations/:id/snooze
pub async fn unsnooze_route(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        if conv_by_local_id(&conn, conversation_id).is_none() {
            return rejected("Conversation not found locally.");
        }
    }
    if let Some(real) = state.real.clone() {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = real
            .request(
                &format!("/v2/conversations/{remote_id}/snooze"),
                "DELETE",
                None,
            )
            .await
        {
            return rejected(&format!("Snooze was NOT removed. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_unsnooze(&conn, conversation_id)
}

/// POST /api/conversations/:id/schedule
pub async fn schedule_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    // Zod collects ALL issues: threadId + scheduledFor (+ optional bool).
    let mut issues: Vec<(&str, String)> = Vec::new();
    let thread_id = match zod_int_msg(&body, "threadId") {
        Ok(v) => Some(v),
        Err(m) => {
            issues.push(("threadId", m));
            None
        }
    };
    let scheduled_for = match zod_string_msg(&body, "scheduledFor") {
        Ok(v) => Some(v),
        Err(m) => {
            issues.push(("scheduledFor", m));
            None
        }
    };
    if !issues.is_empty() {
        let owned: Vec<(&str, &str)> = issues.iter().map(|(p, m)| (*p, m.as_str())).collect();
        return zod_422_multi(&owned);
    }
    let thread_id = thread_id.unwrap_or_default();
    let scheduled_for = scheduled_for.unwrap_or_default();
    let _u = match body.get("unscheduleOnCustomerReply") {
        None | Some(Value::Null) => true,
        _ => match zod_bool_msg(&body, "unscheduleOnCustomerReply") {
            Ok(b) => b,
            Err(m) => return zod_422("unscheduleOnCustomerReply", &m),
        },
    };
    let _ = _u;
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
    }
    if let Some(real) = state.real.clone() {
        let remote_ids = {
            let conn = state.conn_lock();
            let c = conv_by_local_id(&conn, conversation_id);
            let t_remote: Option<i64> = conn
                .query_row(
                    "SELECT remote_id FROM conversation_threads WHERE id = ?1",
                    rusqlite::params![thread_id],
                    |r| r.get(0),
                )
                .ok();
            (c.map(|c| c.remote_id), t_remote)
        };
        match remote_ids {
            (Some(conv_remote), Some(_thread_remote)) => {
                if let Err(e) = real
                    .request(
                        &format!("/v2/conversations/{conv_remote}/threads/{thread_id}/schedule"),
                        "PUT",
                        Some(json!({ "scheduledFor": scheduled_for, "unscheduleOnCustomerReply": _u, "sendAsCreator": false })),
                    )
                    .await
                {
                    return rejected(&format!("Schedule was NOT applied. {e}"));
                }
            }
            _ => return rejected("Conversation or draft thread not found locally."),
        }
    }
    let conn = state.conn_lock();
    op_schedule_reply(&conn, conversation_id, thread_id, &scheduled_for)
}

/// POST /api/conversations/:id/schedule/publish
pub async fn schedule_publish_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Ok(thread_id) = zod_int_msg(&body, "threadId") else {
        return zod_422("threadId", "Required");
    };
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
    }
    if let Some(real) = state.real.clone() {
        let conv_remote: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = real
            .request(
                &format!("/v2/conversations/{conv_remote}/threads/{thread_id}/schedule"),
                "PATCH",
                Some(json!({ "op": "replace", "path": "/state", "value": "published" })),
            )
            .await
        {
            return rejected(&format!("Publish failed. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_publish_schedule(&conn, conversation_id, thread_id)
}

/// DELETE /api/conversations/:id/schedule
pub async fn schedule_delete_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Ok(thread_id) = zod_int_msg(&body, "threadId") else {
        return zod_422("threadId", "Required");
    };
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
    }
    if let Some(real) = state.real.clone() {
        let conv_remote: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = real
            .request(
                &format!("/v2/conversations/{conv_remote}/threads/{thread_id}/schedule"),
                "DELETE",
                None,
            )
            .await
        {
            return rejected(&format!("Delete failed. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_delete_schedule(&conn, conversation_id, thread_id)
}

/// POST /api/conversations/bulk — queue-based bulk actions.
pub async fn bulk_route(State(state): State<AppState>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let Some(ids) = body.get("conversationIds").and_then(|v| v.as_array()) else {
        return zod_422("conversationIds", "Required");
    };
    if ids.is_empty() {
        return zod_422(
            "conversationIds",
            "Array must contain at least 1 element(s)",
        );
    }
    let mut conversation_ids = Vec::new();
    for v in ids {
        match v.as_i64() {
            Some(i) => conversation_ids.push(i),
            None => {
                return zod_422(
                    "conversationIds",
                    &format!(
                        "Expected number, received {}",
                        if v.is_string() { "string" } else { "object" }
                    ),
                )
            }
        }
    }
    let Some(action) = body.get("action").and_then(|v| v.as_str()) else {
        return zod_422("action", "Required");
    };
    const ACTIONS: [&str; 6] = ["tag", "untag", "assign", "unassign", "status", "close"];
    if !ACTIONS.contains(&action) {
        return zod_422("action", &zod_enum_message(&ACTIONS, action));
    }
    let params_body = body.get("params").cloned().unwrap_or_else(|| json!({}));
    let conn = state.conn_lock();
    op_bulk_action(&conn, &conversation_ids, action, &params_body)
}

/// POST /api/conversations/:id/refresh — refresh one conversation via the
/// shared sync coordinator (reference operations.ts refreshOne).
pub async fn refresh_route(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let remote_id = {
        let conn = state.conn_lock();
        match conv_by_local_id(&conn, conversation_id) {
            Some(c) => c.remote_id,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({
                        "statusCode": 404, "error": "NotFound",
                        "message": "Conversation not found locally."
                    })),
                )
                    .into_response()
            }
        }
    };
    // Demo mode: no remote to refresh — the local store IS current.
    if let Some(sync) = state.sync.clone() {
        match sync.sync_single_conversation(remote_id).await {
            Ok(_) => (
                StatusCode::OK,
                Json(json!({ "ok": true, "message": "Conversation refreshed from Help Scout." })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::OK,
                Json(json!({ "ok": false, "message": format!("Refresh failed: {e}") })),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::OK,
            Json(json!({ "ok": true, "message": "Conversation refreshed from Help Scout." })),
        )
            .into_response()
    }
}

/// POST /api/conversations/:id/workflow/:workflowId — run a Help Scout
/// workflow on a conversation.
pub async fn workflow_route(
    State(state): State<AppState>,
    Path((id, workflow_id)): Path<(String, String)>,
) -> Response {
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let workflow_id = match js_number(&workflow_id) {
        Some(f) if f.is_finite() && f.fract() == 0.0 => f as i64,
        Some(_) => return zod_422("workflowId", "Expected integer, received float"),
        None => return zod_422("workflowId", "Expected number, received nan"),
    };
    {
        let conn = state.conn_lock();
        if let Some(msg) = eval_mode_blocked(&conn) {
            return rejected(msg);
        }
        if conv_by_local_id(&conn, conversation_id).is_none() {
            return rejected("Conversation not found locally.");
        }
    }
    if let Some(real) = state.real.clone() {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = real
            .request(
                &format!("/v2/workflows/{workflow_id}/run"),
                "POST",
                Some(json!({ "conversationId": remote_id })),
            )
            .await
        {
            return rejected(&format!("Workflow failed. {e}"));
        }
    }
    let conn = state.conn_lock();
    op_run_workflow(&conn, conversation_id, workflow_id)
}

/// POST /api/attachments/:id/download — fetch attachment bytes to the local
/// attachments directory.
pub async fn attachment_download_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let attachment_id = match js_number(&id) {
        Some(f) if f.is_finite() && f.fract() == 0.0 => f as i64,
        _ => return zod_422("id", "Expected number, received nan"),
    };
    let attachments_dir = state.data_dir.join("attachments");
    let conn = state.conn_lock();
    crate::conversation_ops::op_download_attachment(&conn, &attachments_dir, attachment_id)
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

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    /// Safety invariant (operations.ts evalModeBlocked): with evaluation mode
    /// ON, EVERY remote write is blocked — the five previously-ungated routes
    /// (reply/note/status/assign/subject) must reject with the exact message
    /// BEFORE touching the conversation.
    #[tokio::test]
    async fn eval_mode_blocks_reply_note_status_assign_subject() {
        let state = make_state();
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::settings::set_bool(&conn, "ai_evaluation_mode", true).unwrap();
        }

        let r1 = body_json(
            reply(
                State(state.clone()),
                Path(1),
                Json(json!({"body": "hello"})),
            )
            .await
            .into_response(),
        )
        .await;
        let r2 = body_json(
            note(State(state.clone()), Path(1), Json(json!({"body": "note"})))
                .await
                .into_response(),
        )
        .await;
        let r3 = body_json(
            status(
                State(state.clone()),
                Path(1),
                Json(json!({"status": "closed"})),
            )
            .await
            .into_response(),
        )
        .await;
        let r4 = body_json(
            assign(
                State(state.clone()),
                Path(1),
                Json(json!({"assigneeLocalId": 2})),
            )
            .await
            .into_response(),
        )
        .await;
        let r5 = body_json(
            subject(
                State(state.clone()),
                Path(1),
                Json(json!({"subject": "new"})),
            )
            .await
            .into_response(),
        )
        .await;

        for (name, (code, body)) in [
            ("reply", r1),
            ("note", r2),
            ("status", r3),
            ("assign", r4),
            ("subject", r5),
        ] {
            assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{name} must 422");
            assert_eq!(body["ok"], json!(false), "{name} must be ok:false");
            assert_eq!(
                body["message"],
                json!("AI evaluation mode is ON: no replies, notes, status changes or other updates are sent to Help Scout."),
                "{name} must carry the reference message"
            );
        }
    }

    /// With evaluation mode OFF the gate is silent — the request proceeds
    /// into the pipeline (which handles the missing conversation its own
    /// way; the 422 shape for that path is the write-pipeline port's scope).
    #[tokio::test]
    async fn eval_mode_off_reply_still_works() {
        let state = make_state();
        let (code, body) = body_json(
            reply(
                State(state.clone()),
                Path(999),
                Json(json!({"body": "hello"})),
            )
            .await
            .into_response(),
        )
        .await;
        let msg = body["message"].as_str().unwrap_or("");
        assert_ne!(
            msg,
            "AI evaluation mode is ON: no replies, notes, status changes or other updates are sent to Help Scout.",
            "the gate must not fire when evaluation mode is off"
        );
        assert_ne!(
            code,
            StatusCode::UNPROCESSABLE_ENTITY,
            "no eval 422 when evaluation mode is off"
        );
    }
}
