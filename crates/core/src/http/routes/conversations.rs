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
    let conn = state.conn.lock().expect("mutex poisoned");

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
            Json(json!({
                "conversations": items_json,
                "total": total,
                "page": 1,
                "page_size": 50,
                "view": params.get("view").cloned().unwrap_or_default(),
                "notes": [],
            }))
        }
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
        ),
    }
}

/// GET /api/conversations/:id — conversation detail with threads.
pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    match crate::inbox::get_conversation(&conn, id) {
        Ok(Some(detail)) => match serde_json::to_value(&detail) {
            Ok(v) => Json(v),
            Err(e) => Json(
                json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
            ),
        },
        Ok(None) => Json(
            json!({"_status": 404, "statusCode": 404, "error": "NotFound", "message": "Conversation not found locally."}),
        ),
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
        return Json(
            json!({"_status": 422, "statusCode": 422, "error": "ValidationError", "message": "Reply body cannot be empty."}),
        );
    }
    let mut conn = state.conn.lock().expect("mutex poisoned");
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
                crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
            }
            Json(json!({"ok": ok, "message": "Reply sent."}))
        }
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
    let mut conn = state.conn.lock().expect("mutex poisoned");
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
                crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
            }
            Json(json!({"ok": ok}))
        }
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
    let mut conn = state.conn.lock().expect("mutex poisoned");
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
                crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
            }
            Json(json!({"ok": ok}))
        }
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
    let mut conn = state.conn.lock().expect("mutex poisoned");
    let result = crate::inbox::assign(&mut conn, id, assignee, "user".to_string(), None);
    drop(conn);
    match result {
        Ok(result) => {
            let ok = matches!(result, crate::ticket_ops::OperationResult::Success { .. });
            if ok {
                crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
            }
            Json(json!({"ok": ok}))
        }
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
    let mut conn = state.conn.lock().expect("mutex poisoned");
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
                crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
            }
            Json(json!({"ok": ok}))
        }
        Err(e) => {
            drop(conn);
            Json(
                json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
    let conn = state.conn.lock().expect("mutex poisoned");
    let result = conn.execute(
        "UPDATE conversations SET subject = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE remote_id = ?2",
        rusqlite::params![new_subject, id],
    );
    let updated = result.as_ref().map(|rows| *rows > 0).unwrap_or(false);
    drop(conn);
    if updated {
        crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
    }
    match result {
        Ok(rows) => Json(json!({"ok": rows > 0})),
        Err(e) => Json(
            json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
    let mut conn = state.conn.lock().expect("mutex poisoned");
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
                crate::http::event_bus::notify_sync(&state.bus, "conversations", 1);
            }
            Json(json!({"ok": ok}))
        }
        Err(e) => {
            drop(conn);
            Json(
                json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
            )
        }
    }
}

/// GET /api/conversations/:id/events — activity events for a conversation.
pub async fn events(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = match conn.prepare(
        "SELECT id, conversation_id, event_type, actor_type, actor_id, occurred_at
         FROM activity_events WHERE conversation_id = ?1 ORDER BY occurred_at ASC",
    ) {
        Ok(s) => s,
        Err(e) => {
            return Json(
                json!({"_status": 500, "statusCode": 500, "error": "InternalError", "message": e.to_string()}),
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
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    Json(json!({"events": events}))
}

/// POST /api/conversations/activity/rebuild — rebuild activity events.
pub async fn activity_rebuild(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Activity rebuild queued."}))
}

/// GET /api/ticket-states — list custom ticket states.
pub async fn list_ticket_states(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = match conn.prepare("SELECT id, name, color FROM ticket_states ORDER BY id") {
        Ok(s) => s,
        Err(_) => {
            return Json(json!({"states": [], "bottlenecks": []}));
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
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    Json(json!({"states": states, "bottlenecks": []}))
}
