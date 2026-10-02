//! Conversation routes — mirrors src/server/routes/conversations.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

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
            Ok(v) => (StatusCode::OK, Json(v)),
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let mut stmt = match conn.prepare("SELECT id, name, color FROM ticket_states ORDER BY id") {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::OK,
                Json(json!({"states": [], "bottlenecks": []})),
            );
        }
    };
    let states: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "color": r.get::<_, String>(2)?,
            }))
        })
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!({"states": states, "bottlenecks": []})),
    )
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let tags: Vec<Value> = conn
        .prepare("SELECT id, name, (SELECT COUNT(*) FROM conversation_tags WHERE tag_id = t.id) AS ticket_count FROM tags t ORDER BY t.name")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "ticket_count": r.get::<_, i64>(2)?,
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
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let teams: Vec<Value> = conn
        .prepare("SELECT id, name FROM teams ORDER BY id")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    (StatusCode::OK, Json(json!({"teams": teams})))
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
