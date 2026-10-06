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

/// C2 (T16 audit): the 500 envelope for a failed local DB write — a mutation
/// error must surface as this, never as `ok:true` + SSE (fake success).
fn db_error_500(e: &rusqlite::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "statusCode": 500,
            "error": "InternalError",
            "message": e.to_string()
        })),
    )
        .into_response()
}

/// GET /api/conversations — list conversations with filters.
///
/// v1.8.0 Operations Center drill-down: `?ops=<tileKey>` compiles to the
/// SAME whitelisted fragment the tile count uses, so a tile can never
/// disagree with its list (reference conversations.ts:81-93).
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
    // v2.0.0 (M4): exact number lookup (?number=N) for deep links and the
    // incident link-by-number flow; garbage is ignored, never a 500.
    if let Some(number) = params.get("number") {
        filters.number = number.trim().parse::<i64>().ok();
    }
    if let Some(limit) = params.get("pageSize") {
        filters.limit = limit.parse().ok();
    }
    let mut notes: Vec<String> = Vec::new();
    if let Some(ops) = params.get("ops").filter(|ops| !ops.is_empty()) {
        if !crate::operations::is_conversation_ops_tile(ops) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": crate::operations::OPS_INVALID_MESSAGE,
                })),
            );
        }
        let threshold = crate::operations::waiting_threshold_minutes(&conn);
        let Some((frag_sql, frag_params)) = crate::operations::tile_fragment(ops, threshold) else {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": crate::operations::OPS_INVALID_MESSAGE,
                })),
            );
        };
        filters.extra_where = Some(frag_sql);
        filters.extra_params = frag_params;
        notes.push(format!("Operations Center tile '{ops}' applied."));
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
                    "notes": notes,
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

/// POST /api/conversations/:id/reply (conversations.ts:343) — the sendReply
/// write pipeline: Zod parse (replyRequestSchema, conversationId injected
/// from :id) → ops.sendReply. Not-connected maps to 503; every other
/// ok:false is 422.
pub async fn reply(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let (text, draft, cc, bcc, status_after, assign_to) = match parse_reply_body(&body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    crate::conversation_ops::op_send_reply(
        &state,
        crate::conversation_ops::ReplyInput {
            conversation_id,
            text,
            draft,
            cc,
            bcc,
            status_after,
            assign_to,
        },
    )
    .await
}

/// POST /api/conversations/:id/note (conversations.ts:354) — the addNote
/// write pipeline (noteRequestSchema).
pub async fn note(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let text = match parse_text_min1(&body, "text") {
        Ok(v) => v,
        Err(r) => return r,
    };
    crate::conversation_ops::op_add_note(&state, conversation_id, &text, false).await
}

/// POST /api/conversations/:id/status (conversations.ts:362) — the
/// changeStatus write pipeline (statusRequestSchema).
pub async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    const STATUSES: [&str; 4] = ["active", "closed", "pending", "spam"];
    let new_status = match body.get("status") {
        Some(Value::String(s)) => {
            if !STATUSES.contains(&s.as_str()) {
                return zod_422("status", &zod_enum_message(&STATUSES, s));
            }
            s.clone()
        }
        None | Some(Value::Null) => return zod_422("status", "Required"),
        Some(v) => {
            return zod_422(
                "status",
                &format!("Expected string, received {}", zod_type_name(v)),
            )
        }
    };
    crate::conversation_ops::op_change_status(&state, conversation_id, &new_status).await
}

/// POST /api/conversations/:id/assign (conversations.ts:370) — the assign
/// write pipeline (assignRequestSchema; userId is a REMOTE id, nullable).
pub async fn assign(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let user_id = match parse_required_nullable_int(&body, "userId") {
        Ok(v) => v,
        Err(r) => return r,
    };
    crate::conversation_ops::op_assign(&state, conversation_id, user_id).await
}

/// POST /api/conversations/:id/priority (conversations.ts:279) — the
/// reference setPriority: a LOCAL SupportOS write (the optional Help Scout
/// custom-field mapping defaults OFF, so no eval gate on this path — the
/// gate only guards the remote-write branch we never take by default).
pub async fn priority(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    const PRIORITIES: [&str; 5] = ["none", "low", "medium", "high", "urgent"];
    let priority_str = match body.get("priority") {
        Some(Value::String(s)) => {
            if !PRIORITIES.contains(&s.as_str()) {
                return zod_422("priority", &zod_enum_message(&PRIORITIES, s));
            }
            s.clone()
        }
        None | Some(Value::Null) => return zod_422("priority", "Required"),
        Some(v) => {
            return zod_422(
                "priority",
                &format!("Expected string, received {}", zod_type_name(v)),
            )
        }
    };
    let mut conn = state.conn_lock();
    let Some(conv) = crate::conversation_ops::conv_full_by_local_id(&conn, conversation_id) else {
        return rejected("Conversation not found locally.");
    };
    let previous: Option<String> = conn
        .query_row(
            "SELECT supportos_priority FROM conversations WHERE id = ?1",
            rusqlite::params![conv.id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // Local write + transition record (one transaction, like the reference).
    // C2 (T16 audit): every statement is CHECKED — a DB error must surface
    // as the 500 envelope, never as ok:true + SSE. A failed statement
    // returns early, dropping the uncommitted transaction (automatic
    // rollback), so the priority write + activity pair stay atomic.
    let tx = match conn.transaction() {
        Ok(tx) => tx,
        Err(e) => return db_error_500(&e),
    };
    if let Err(e) = tx.execute(
        "UPDATE conversations SET supportos_priority = ?1 WHERE id = ?2",
        rusqlite::params![priority_str, conv.id],
    ) {
        return db_error_500(&e);
    }
    if let Err(e) = tx.execute(
        "INSERT OR IGNORE INTO activity_events (conversation_id, event_type,
             actor_type, actor_id, occurred_at, source, metadata, dedup_key)
         VALUES (?1, 'priority_changed', 'user', NULL, ?2, 'local', ?3,
             'priority_changed:' || ?1 || ':' || ?2)",
        rusqlite::params![
            conv.id,
            at,
            json!({ "previous": previous, "next": priority_str, "hs_field_synced": false })
                .to_string()
        ],
    ) {
        return db_error_500(&e);
    }
    if let Err(e) = tx.commit() {
        return db_error_500(&e);
    }
    let _ = crate::jobs::audit(
        &conn,
        "user",
        "priority_changed",
        Some(conv.id),
        Some(&json!({ "priority": previous }).to_string()),
        Some(&json!({ "priority": priority_str, "hs_field_synced": false }).to_string()),
        None,
        None,
        false,
    );
    drop(conn);
    crate::http::event_bus::notify_conversation_updated(
        &state.bus,
        &crate::events::ConversationUpdatedEvent {
            conversation_id: Some(conv.id),
            conversation_number: None,
            mailbox_id: None,
            subject: None,
            reason: "sync".into(),
            at: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        },
    );
    ok_result(
        &format!("Priority set to {priority_str}."),
        Some(json!({ "priority": priority_str, "hs_field_synced": false })),
    )
}

/// POST /api/conversations/:id/subject (conversations.ts:378) — the
/// changeSubject write pipeline (subjectRequestSchema).
pub async fn subject(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let new_subject = match parse_text_min1(&body, "subject") {
        Ok(v) => v,
        Err(r) => return r,
    };
    crate::conversation_ops::op_change_subject(&state, conversation_id, &new_subject).await
}

/// POST /api/conversations/:id/state (conversations.ts:287) — set the
/// SupportOS custom ticket state (setStateRequestSchema: stateId int
/// positive nullable, reason string max 500 optional). Purely local:
/// transition history + activity event + audit.
pub async fn set_state(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let conversation_id = match path_id(&id) {
        Ok(v) => v,
        Err(r) => return r,
    };
    // stateId: z.number().int().positive().nullable()
    let state_id: Option<i64> = match body.get("stateId") {
        None => return zod_422("stateId", "Required"),
        Some(Value::Null) => None,
        Some(v) => match zod_int_value(v) {
            Ok(n) if n > 0 => Some(n),
            Ok(_) => return zod_422("stateId", "Number must be greater than 0"),
            Err(m) => return zod_422("stateId", &m),
        },
    };
    // reason: z.string().max(500).optional()
    let reason: Option<String> = match body.get("reason") {
        None => None,
        Some(Value::String(s)) => {
            if s.chars().count() > 500 {
                return zod_422("reason", "String must contain at most 500 character(s)");
            }
            Some(s.clone())
        }
        Some(Value::Null) => return zod_422("reason", "Expected string, received null"),
        Some(v) => {
            return zod_422(
                "reason",
                &format!("Expected string, received {}", zod_type_name(v)),
            )
        }
    };
    let mut conn = state.conn_lock();
    let Some(conv) = crate::conversation_ops::conv_full_by_local_id(&conn, conversation_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Conversation not found locally."
            })),
        )
            .into_response();
    };
    let current: Option<i64> = conn
        .query_row(
            "SELECT supportos_state_id FROM conversations WHERE id = ?1",
            rusqlite::params![conv.id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let state_name: Option<String> = match state_id {
        Some(sid) => conn
            .query_row(
                "SELECT name FROM ticket_states WHERE id = ?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .ok(),
        None => None,
    };
    if state_id.is_some() && state_name.is_none() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "ok": false,
                "message": "State not found.",
                "previousStateId": current,
            })),
        )
            .into_response();
    }
    if current == state_id {
        return (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "message": "Conversation already in this state.",
                "previousStateId": current,
            })),
        )
            .into_response();
    }
    let occurred_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    // C2 (T16 audit): every statement is CHECKED — a DB error must surface
    // as the 500 envelope, never as ok:true + SSE. A failed statement
    // returns early, dropping the uncommitted transaction (automatic
    // rollback), so the transition + conversation update + activity entry
    // stay atomic: either all three land or none does.
    let tx = match conn.transaction() {
        Ok(tx) => tx,
        Err(e) => return db_error_500(&e),
    };
    if let Err(e) = tx.execute(
        "INSERT INTO state_transitions (conversation_id, previous_state_id,
             new_state_id, actor_type, actor_local_id, reason, occurred_at, source)
         VALUES (?1, ?2, ?3, 'user', NULL, ?4, ?5, 'local')",
        rusqlite::params![conv.id, current, state_id, reason, occurred_at],
    ) {
        return db_error_500(&e);
    }
    let transition_rowid = tx.last_insert_rowid();
    if let Err(e) = tx.execute(
        "UPDATE conversations SET supportos_state_id = ?1 WHERE id = ?2",
        rusqlite::params![state_id, conv.id],
    ) {
        return db_error_500(&e);
    }
    // Activity entry: dedup key carries the TRANSITION rowid so two
    // changes in the same millisecond stay distinct.
    if let Err(e) = tx.execute(
        "INSERT OR IGNORE INTO activity_events (conversation_id, event_type,
             actor_type, actor_id, occurred_at, source, metadata, dedup_key)
         VALUES (?1, 'ticket_state_changed', 'user', NULL, ?2, 'local', ?3,
             'ticket_state_changed:' || ?1 || ':' || ?4)",
        rusqlite::params![
            conv.id,
            occurred_at,
            json!({
                "previous_state_id": current,
                "new_state_id": state_id,
                "reason": reason,
            })
            .to_string(),
            transition_rowid
        ],
    ) {
        return db_error_500(&e);
    }
    if let Err(e) = tx.commit() {
        return db_error_500(&e);
    }
    let _ = crate::jobs::audit(
        &conn,
        "user",
        "ticket_state_changed",
        Some(conv.id),
        Some(&json!({ "state_id": current }).to_string()),
        Some(&json!({ "state_id": state_id, "reason": reason }).to_string()),
        None,
        None,
        false,
    );
    drop(conn);
    crate::http::event_bus::notify_conversation_updated(
        &state.bus,
        &crate::events::ConversationUpdatedEvent {
            conversation_id: Some(conv.id),
            conversation_number: None,
            mailbox_id: None,
            subject: None,
            reason: "sync".into(),
            at: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        },
    );
    let message = match (&state_name, state_id) {
        (Some(name), _) => format!("State set to {name}."),
        (None, Some(_)) => "State set.".to_string(),
        (None, None) => "State cleared.".to_string(),
    };
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": message,
            "previousStateId": current,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Zod request parsers (schemas.ts:302-336, activity.ts:379-386)
// ---------------------------------------------------------------------------

/// Zod v3's `z.string().email()` check. The upstream regex uses two
/// look-aheads the `regex` crate does not support, so they are checked
/// directly: (1) the address must not start with '.', (2) no two
/// consecutive dots anywhere.
const ZOD_EMAIL_RE: &str =
    r"^([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$";

fn zod_email_ok(email: &str) -> bool {
    if email.starts_with('.') || email.contains("..") {
        return false;
    }
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(ZOD_EMAIL_RE).expect("static email regex"))
        .is_match(email)
}

/// The JSON type name Zod reports in `Expected T, received X` messages.
fn zod_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

/// `z.number().int()` on an already-extracted value.
fn zod_int_value(v: &Value) -> std::result::Result<i64, String> {
    if let Some(f) = v.as_f64() {
        if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
            return Ok(f as i64);
        }
        return Err("Expected integer, received float".into());
    }
    Err(format!("Expected number, received {}", zod_type_name(v)))
}

/// `z.string().min(1)`.
fn parse_text_min1(body: &Value, field: &str) -> Result<String, Response> {
    match crate::conversation_ops::zod_string_msg(body, field) {
        Ok(t) if t.is_empty() => Err(zod_422(
            field,
            "String must contain at least 1 character(s)",
        )),
        Ok(t) => Ok(t),
        Err(m) => Err(zod_422(field, &m)),
    }
}

/// `z.array(z.string().email()).default([])` — the default applies to a
/// MISSING key only; null is a type error.
fn parse_email_array(body: &Value, field: &str) -> Result<Vec<String>, Response> {
    match body.get(field) {
        None => Ok(vec![]),
        Some(Value::Null) => Err(zod_422(field, "Expected array, received null")),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, v) in items.iter().enumerate() {
                let Some(s) = v.as_str() else {
                    return Err(zod_422(
                        &format!("{field}[{i}]"),
                        &format!("Expected string, received {}", zod_type_name(v)),
                    ));
                };
                if !zod_email_ok(s) {
                    return Err(zod_422(&format!("{field}[{i}]"), "Invalid email"));
                }
                out.push(s.to_string());
            }
            Ok(out)
        }
        Some(v) => Err(zod_422(
            field,
            &format!("Expected array, received {}", zod_type_name(v)),
        )),
    }
}

/// `z.number().int().nullish()`.
fn parse_optional_int(body: &Value, field: &str) -> Result<Option<i64>, Response> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => zod_int_value(v).map(Some).map_err(|m| zod_422(field, &m)),
    }
}

/// `z.number().int().nullable()` — required key, null allowed.
fn parse_required_nullable_int(body: &Value, field: &str) -> Result<Option<i64>, Response> {
    match body.get(field) {
        None => Err(zod_422(field, "Required")),
        Some(Value::Null) => Ok(None),
        Some(v) => zod_int_value(v).map(Some).map_err(|m| zod_422(field, &m)),
    }
}

/// replyRequestSchema (schemas.ts:302) minus conversationId (injected from
/// the path by the handler): text min 1, draft default false, cc/bcc email
/// arrays default [], statusAfter enum nullish, assignTo int nullish,
/// attachmentIds int array default [] (validated only — sendReply never
/// forwards attachments to the provider).
fn parse_reply_body(
    body: &Value,
) -> Result<
    (
        String,
        bool,
        Vec<String>,
        Vec<String>,
        Option<String>,
        Option<i64>,
    ),
    Response,
> {
    let text = parse_text_min1(body, "text")?;
    let draft = match body.get("draft") {
        None => false,
        Some(Value::Null) => return Err(zod_422("draft", "Expected boolean, received null")),
        Some(_) => match crate::conversation_ops::zod_bool_msg(body, "draft") {
            Ok(b) => b,
            Err(m) => return Err(zod_422("draft", &m)),
        },
    };
    let cc = parse_email_array(body, "cc")?;
    let bcc = parse_email_array(body, "bcc")?;
    const STATUS_AFTER: [&str; 6] = [
        "active",
        "closed",
        "pending",
        "spam",
        "open",
        "inbox_predefined",
    ];
    let status_after = match body.get("statusAfter") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            if !STATUS_AFTER.contains(&s.as_str()) {
                return Err(zod_422("statusAfter", &zod_enum_message(&STATUS_AFTER, s)));
            }
            Some(s.clone())
        }
        Some(v) => {
            return Err(zod_422(
                "statusAfter",
                &format!("Expected string, received {}", zod_type_name(v)),
            ))
        }
    };
    let assign_to = parse_optional_int(body, "assignTo")?;
    match body.get("attachmentIds") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for (i, v) in items.iter().enumerate() {
                if let Err(m) = zod_int_value(v) {
                    return Err(zod_422(&format!("attachmentIds[{i}]"), &m));
                }
            }
        }
        Some(v) => {
            return Err(zod_422(
                "attachmentIds",
                &format!("Expected array, received {}", zod_type_name(v)),
            ))
        }
    }
    Ok((text, draft, cc, bcc, status_after, assign_to))
}

/// GET /api/conversations/:id/events — the full event timeline for one
/// conversation (v1.7.0; reference conversations.ts:265-272 +
/// `activityRepo.listEvents`/`eventCounts`, audit AC-04): 404 when the
/// conversation is not in the mirror; `limit` clamps 1..=1000 (fallback 200);
/// every event carries its actor NAME resolved through
/// users/customers/system_users by actor type, its metadata parsed from the
/// stored JSON (non-objects collapse to `{}` like the reference `safeParse`),
/// the thread link and the source; `counts` summarizes events per type for
/// the UI chips.
///
/// Route shape (reference): `{ conversation_id, events, counts }`.
pub async fn events(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    // clampListParam(q.limit, 200, 1, 1000): Number(value), NaN/garbage falls
    // back to the default, then clamps [min, max] after truncation.
    let limit: i64 = match params
        .get("limit")
        .filter(|s| !s.is_empty())
        .and_then(|s| crate::conversation_ops::js_number(s))
    {
        Some(n) if n.is_finite() => (n.trunc() as i64).clamp(1, 1000),
        _ => 200,
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());

    // 404 when the conversation is not in the mirror (reference
    // repo.getConversationByLocalId — a plain existence check).
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?1",
            rusqlite::params![id],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Conversation not found locally."
            })),
        )
            .into_response();
    }

    // Chronological timeline with actor names resolved (reference
    // listEvents, activityRepo.ts:172-191).
    let events: Vec<Value> = match conn.prepare(
        "SELECT e.id, e.conversation_id, e.thread_local_id, e.event_type, e.actor_type, e.actor_id,
                e.occurred_at, e.source, e.metadata, e.created_at,
                CASE
                  WHEN e.actor_type = 'user' THEN
                    (SELECT TRIM(COALESCE(u.first_name, '') || ' ' || COALESCE(u.last_name, ''))
                     FROM users u WHERE u.id = e.actor_id)
                  WHEN e.actor_type = 'customer' THEN
                    (SELECT TRIM(COALESCE(cu.first_name, '') || ' ' || COALESCE(cu.last_name, ''))
                     FROM customers cu WHERE cu.id = e.actor_id)
                  WHEN e.actor_type = 'system_user' THEN
                    (SELECT TRIM(COALESCE(su.first_name, '') || ' ' || COALESCE(su.last_name, ''))
                     FROM system_users su WHERE su.id = e.actor_id)
                  ELSE NULL
                END AS actor_name
         FROM activity_events e
         WHERE e.conversation_id = ?1
         ORDER BY COALESCE(e.occurred_at, e.created_at) ASC, e.id ASC
         LIMIT ?2",
    ) {
        Ok(mut stmt) => stmt
            .query_map(rusqlite::params![id, limit], |r| {
                // safeParse: a stored metadata string parses to an object or
                // collapses to {} (never an error).
                let metadata = r
                    .get::<_, Option<String>>(8)?
                    .and_then(|m| serde_json::from_str::<Value>(&m).ok())
                    .filter(|v| v.is_object())
                    .unwrap_or_else(|| json!({}));
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, i64>(1)?,
                    "thread_local_id": r.get::<_, Option<i64>>(2)?,
                    "event_type": r.get::<_, String>(3)?,
                    "actor_type": r.get::<_, Option<String>>(4)?,
                    "actor_local_id": r.get::<_, Option<i64>>(5)?,
                    "occurred_at": r.get::<_, Option<String>>(6)?,
                    "source": r.get::<_, Option<String>>(7)?,
                    "metadata": metadata,
                    "created_at": r.get::<_, Option<String>>(9)?,
                    "actor_name": r.get::<_, Option<String>>(10)?,
                }))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"statusCode": 500, "error": "InternalError", "message": e.to_string()}),
                ),
            )
                .into_response();
        }
    };

    // Count events per type (UI summary chips; reference eventCounts).
    let count_pairs: Vec<(String, i64)> = conn
        .prepare(
            "SELECT event_type, COUNT(*) FROM activity_events
             WHERE conversation_id = ?1 GROUP BY event_type",
        )
        .and_then(|mut stmt| {
            stmt.query_map(rusqlite::params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    let mut counts = serde_json::Map::new();
    for (kind, n) in count_pairs {
        counts.insert(kind, json!(n));
    }

    (
        StatusCode::OK,
        Json(json!({
            "conversation_id": id,
            "events": events,
            "counts": Value::Object(counts),
        })),
    )
        .into_response()
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
    conv_by_local_id, eval_mode_blocked, js_number, ok_result, op_bulk_action, op_delete_schedule,
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
    // SY-10: fresh-read-merge-write through the PROVIDER boundary
    // (operations.ts updateTags:269-303) — the fake provider serves its world
    // state, the real provider hits the v2 API, so demo mode now behaves like
    // the remote instead of skipping the provider write entirely.
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Ok(Some(remote)) = provider.get_conversation(remote_id).await {
            let current = remote.tags.clone();
            let desired = crate::conversation_ops::merge_tags(&current, &change);
            if let Err(e) = provider.update_tags(remote_id, desired).await {
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
    // SY-10: provider boundary with the reference's fresh-read merge
    // (operations.ts updateCustomFields:307-341): system fields are
    // preserved, user fields merge with the change set, and the complete
    // state is PUT.
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Ok(Some(remote)) = provider.get_conversation(remote_id).await {
            let changed_ids: std::collections::HashSet<i64> =
                parsed.iter().map(|(id, _)| *id).collect();
            let mut merged: Vec<(i64, Option<String>)> = Vec::new();
            for f in &remote.custom_fields {
                if f.system_type.is_some() {
                    merged.push((f.field_id, f.value.clone()));
                } else if !changed_ids.contains(&f.field_id) {
                    merged.push((f.field_id, f.value.clone()));
                }
            }
            merged.extend(parsed.iter().cloned());
            if let Err(e) = provider.update_custom_fields(remote_id, merged).await {
                return rejected(&format!("Fields were NOT changed. {e}"));
            }
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
    // SY-10: provider boundary (operations.ts snooze:345-364).
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = provider
            .snooze_conversation(remote_id, snoozed_until.clone(), _unsnooze)
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
    // SY-10: provider boundary (operations.ts unsnooze:366-382).
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = provider.unsnooze_conversation(remote_id).await {
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
    // SY-10: provider boundary with the thread's REMOTE id
    // (operations.ts scheduleReply:384-405 — the port previously sent the
    // LOCAL thread id in the v2 URL).
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
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
            (Some(conv_remote), Some(thread_remote)) => {
                if let Err(e) = provider
                    .schedule_thread(conv_remote, thread_remote, scheduled_for.clone(), _u)
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
    // SY-10: provider boundary (operations.ts publishSchedule:407-428).
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let (conv_remote, thread_remote): (i64, Option<i64>) = {
            let conn = state.conn_lock();
            let c = conv_by_local_id(&conn, conversation_id).map(|c| c.remote_id);
            let t = conn
                .query_row(
                    "SELECT remote_id FROM conversation_threads WHERE id = ?1",
                    rusqlite::params![thread_id],
                    |r| r.get(0),
                )
                .ok();
            (c.unwrap_or_default(), t)
        };
        let Some(thread_remote) = thread_remote else {
            return rejected("Conversation or thread not found locally.");
        };
        if let Err(e) = provider
            .publish_scheduled_thread(conv_remote, thread_remote)
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
    // SY-10: provider boundary (operations.ts deleteSchedule:430-448).
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let (conv_remote, thread_remote): (i64, Option<i64>) = {
            let conn = state.conn_lock();
            let c = conv_by_local_id(&conn, conversation_id).map(|c| c.remote_id);
            let t = conn
                .query_row(
                    "SELECT remote_id FROM conversation_threads WHERE id = ?1",
                    rusqlite::params![thread_id],
                    |r| r.get(0),
                )
                .ok();
            (c.unwrap_or_default(), t)
        };
        let Some(thread_remote) = thread_remote else {
            return rejected("Conversation or thread not found locally.");
        };
        if let Err(e) = provider
            .delete_thread_schedule(conv_remote, thread_remote)
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
    // SY-10: provider boundary (operations.ts runWorkflow:521-533). The
    // reference body is `conversationIds: [id]` (services.ts:347) — the
    // port's old inline call sent `conversationId`, which Help Scout ignores.
    if let Some(provider) = crate::conversation_ops::op_provider(&state) {
        let remote_id: i64 = {
            let conn = state.conn_lock();
            conv_by_local_id(&conn, conversation_id)
                .map(|c| c.remote_id)
                .unwrap_or_default()
        };
        if let Err(e) = provider.run_workflow(workflow_id, remote_id).await {
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
    crate::conversation_ops::op_download_attachment(&state, &attachments_dir, attachment_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
    use std::sync::{Arc, Mutex};

    pub(super) fn make_state() -> AppState {
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

    /// A state wired the way the real app wires it: one fake provider shared
    /// by the sync engine and the ops (demo mode), with the demo world
    /// mirrored into the DB so conversations exist locally.
    pub(super) async fn make_pipeline_state() -> AppState {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("pipeline.db");
        let mut conn = crate::db::open(&db_path).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        let http_conn = Arc::new(Mutex::new(conn));
        let bus = crate::http::EventBus::new(64);
        let provider = Arc::new(crate::helpscout::FakeHelpScoutProvider::new_demo())
            as Arc<dyn crate::helpscout::HelpScoutProvider>;
        let sync = Arc::new(
            crate::sync_engine::SyncEngine::new(http_conn.clone(), provider).with_bus(bus.clone()),
        );
        sync.initial_sync().await.expect("demo initial sync");
        AppState {
            conn: http_conn,
            data_dir: tmp.path().to_path_buf(),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: true,
            bus,
            limiter: crate::http::RateLimiter::new(),
            sync: Some(sync),
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

    /// (local id, remote id) of the first mirrored conversation.
    fn first_conversation(state: &AppState) -> (i64, i64) {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT id, remote_id FROM conversations ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
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
    /// ON, every REMOTE write route rejects with its exact reference message
    /// BEFORE touching the provider. Note: addNote checks the conversation
    /// BEFORE the eval flag (reference order), so it needs a real row.
    #[tokio::test]
    async fn eval_mode_blocks_reply_note_status_assign_subject() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::settings::set_bool(&conn, "ai_evaluation_mode", true).unwrap();
        }

        let cases: Vec<(&str, Response)> = vec![
            (
                "reply",
                reply(
                    State(state.clone()),
                    Path(id.to_string()),
                    Some(Json(json!({"text": "hello"}))),
                )
                .await
                .into_response(),
            ),
            (
                "note",
                note(
                    State(state.clone()),
                    Path(id.to_string()),
                    Some(Json(json!({"text": "note"}))),
                )
                .await
                .into_response(),
            ),
            (
                "status",
                status(
                    State(state.clone()),
                    Path(id.to_string()),
                    Some(Json(json!({"status": "closed"}))),
                )
                .await
                .into_response(),
            ),
            (
                "assign",
                assign(
                    State(state.clone()),
                    Path(id.to_string()),
                    Some(Json(json!({"userId": 2}))),
                )
                .await
                .into_response(),
            ),
            (
                "subject",
                subject(
                    State(state.clone()),
                    Path(id.to_string()),
                    Some(Json(json!({"subject": "new"}))),
                )
                .await
                .into_response(),
            ),
        ];
        // The reference messages differ per op (operations.ts).
        let expected = [
            "AI evaluation mode is ON: no replies, notes, status changes or other updates are sent to Help Scout.",
            "AI evaluation mode is ON: no notes, replies or status changes are sent to Help Scout.",
            "AI evaluation mode is ON: status changes are disabled.",
            "AI evaluation mode is ON: no replies, notes, status changes or other updates are sent to Help Scout.",
            "AI evaluation mode is ON: no replies, notes, status changes or other updates are sent to Help Scout.",
        ];
        for ((name, resp), msg) in cases.into_iter().zip(expected.iter()) {
            let (code, body) = body_json(resp).await;
            assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{name} must 422");
            assert_eq!(body["ok"], json!(false), "{name} must be ok:false");
            assert_eq!(&body["message"], &json!(msg), "{name} message");
        }
        // Nothing was written: no jobs, no threads beyond the seeded ones.
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let jobs: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbound_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(jobs, 0, "eval mode must not create outbound jobs");
    }

    /// The five routes return the reference 422 `{ok:false}` envelope for a
    /// missing conversation — never a 500 (the old defect).
    #[tokio::test]
    async fn missing_conversation_is_422_not_500() {
        let state = make_state();
        let (code, body) = body_json(
            reply(
                State(state.clone()),
                Path("999".into()),
                Some(Json(json!({"text": "hello"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["message"], json!("Conversation not found locally."));

        for resp in [
            note(
                State(state.clone()),
                Path("999".into()),
                Some(Json(json!({"text": "n"}))),
            )
            .await
            .into_response(),
            status(
                State(state.clone()),
                Path("999".into()),
                Some(Json(json!({"status": "closed"}))),
            )
            .await
            .into_response(),
            assign(
                State(state.clone()),
                Path("999".into()),
                Some(Json(json!({"userId": 1}))),
            )
            .await
            .into_response(),
            subject(
                State(state.clone()),
                Path("999".into()),
                Some(Json(json!({"subject": "s"}))),
            )
            .await
            .into_response(),
        ] {
            let (code, body) = body_json(resp).await;
            assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body["message"], json!("Conversation not found locally."));
        }
    }

    /// Real mode without a connected token: the reply route answers 503 with
    /// the reference message (only the reply route maps not-connected to 503).
    #[tokio::test]
    async fn not_connected_reply_is_503() {
        let state = make_state();
        {
            let mut s = state;
            s.provider_kind = "real".into();
            let (code, body) = body_json(
                reply(
                    State(s),
                    Path("1".into()),
                    Some(Json(json!({"text": "hello"}))),
                )
                .await
                .into_response(),
            )
            .await;
            assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["ok"], json!(false));
            assert_eq!(
                body["message"],
                json!("Help Scout is not connected. Remote actions are disabled - connect Help Scout in Settings first.")
            );
        }
    }

    /// Zod contracts: replyRequestSchema / statusRequestSchema /
    /// assignRequestSchema produce the reference 422 issues.
    #[tokio::test]
    async fn zod_validation_shapes() {
        let state = make_state();
        // empty text
        let (code, body) = body_json(
            reply(
                State(state.clone()),
                Path("1".into()),
                Some(Json(json!({"text": ""}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            json!("Invalid request (text): String must contain at least 1 character(s)")
        );
        // invalid email in cc
        let (code, _body) = body_json(
            reply(
                State(state.clone()),
                Path("1".into()),
                Some(Json(json!({"text": "hi", "cc": ["notanemail"]}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        // bad status enum
        let (code, body) = body_json(
            status(
                State(state.clone()),
                Path("1".into()),
                Some(Json(json!({"status": "resolved"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["message"],
            json!("Invalid request (status): Invalid enum value. Expected 'active' | 'closed' | 'pending' | 'spam', received 'resolved'")
        );
        // assign without the required userId key
        let (code, body) = body_json(
            assign(
                State(state.clone()),
                Path("1".into()),
                Some(Json(json!({}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["message"], json!("Invalid request (userId): Required"));
    }

    /// sendReply happy path in demo mode: provider write (the world) →
    /// single-conversation refresh → audit + idempotency-keyed job. The
    /// thread must appear in GET detail and carry the demo agent as actor.
    #[tokio::test]
    async fn reply_full_pipeline_demo() {
        let state = make_pipeline_state().await;
        let (id, remote) = first_conversation(&state);
        let (code, body) = body_json(
            reply(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"text": "Hello from the pipeline"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "body: {body}");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["message"], json!("Reply sent successfully."));
        let thread_remote_id = body["data"]["threadRemoteId"].as_i64().unwrap();
        assert!(thread_remote_id > 0);

        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        // Thread visible locally with the right shape.
        let (kind, body_text, actor_type): (String, String, String) = conn
            .query_row(
                "SELECT thread_type, body, actor_type FROM conversation_threads
                  WHERE remote_id = ?1",
                rusqlite::params![thread_remote_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "reply");
        assert_eq!(body_text, "Hello from the pipeline");
        assert_eq!(actor_type, "user");
        // Job confirmed with the sha256 idempotency key.
        let (kind, status, key): (String, String, String) = conn
            .query_row(
                "SELECT kind, status, idempotency_key FROM outbound_jobs
                  WHERE idempotency_key IS NOT NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "create_reply");
        assert_eq!(status, "confirmed");
        assert!(
            key.starts_with(&format!("reply:{remote}:send:")),
            "key: {key}"
        );
        // Audit row.
        let (action, job_id_set): (String, i64) = conn
            .query_row(
                "SELECT action, COALESCE(job_id, 0) FROM audit_log
                  WHERE action = 'reply_sent' AND conversation_id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(action, "reply_sent");
        assert!(job_id_set > 0);
        // No phantom "[reply sent: ...]" note (the old double-write).
        let phantoms: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_threads WHERE body LIKE '[reply sent:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(phantoms, 0);
    }

    /// Duplicate-send protection: the same non-draft text twice is rejected;
    /// draft-then-send of the same text is the normal flow and allowed.
    #[tokio::test]
    async fn reply_idempotency() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        let first = body_json(
            reply(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"text": "Once only"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(first.0, StatusCode::OK);
        let second = body_json(
            reply(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"text": "Once only"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(second.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(second.1["message"], json!("This exact reply was already sent (duplicate-send protection). Check the conversation history before sending again."));
        // draft then send of the same text is NOT a duplicate.
        let draft = body_json(
            reply(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"text": "Draft me", "draft": true}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(draft.0, StatusCode::OK);
        assert_eq!(draft.1["message"], json!("Draft saved to Help Scout."));
        let send = body_json(
            reply(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"text": "Draft me"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(send.0, StatusCode::OK);
        assert_eq!(send.1["message"], json!("Reply sent successfully."));
    }

    /// addNote: thread lands with type note, job confirmed, audit actor user.
    #[tokio::test]
    async fn note_full_pipeline_demo() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        let (code, body) = body_json(
            note(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"text": "Internal observation"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["message"], json!("Internal note added."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let (kind, status): (String, String) = conn
            .query_row(
                "SELECT kind, status FROM outbound_jobs WHERE kind = 'create_note'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "create_note");
        assert_eq!(status, "confirmed");
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'note_added'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audited, 1);
    }

    /// changeStatus: closing stamps closed_at atomically; audit records
    /// before/after; the world and the local row agree afterwards.
    #[tokio::test]
    async fn status_close_stamps_closed_at() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        let (code, body) = body_json(
            status(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"status": "closed"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["message"], json!("Status changed to closed."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let (st, closed): (String, Option<String>) = conn
            .query_row(
                "SELECT status, closed_at FROM conversations WHERE id = ?1",
                rusqlite::params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(st, "closed");
        assert!(closed.is_some(), "closed_at must be stamped");
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'status_changed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audited, 1);
        // No phantom system thread ("Status changed to: ...").
        let phantoms: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_threads
                  WHERE thread_type = 'system' AND body LIKE 'Status changed to:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(phantoms, 0);
    }

    /// assign: a REMOTE user id resolves through the mirror to the local
    /// assignee; null unassigns. Both messages match the reference.
    #[tokio::test]
    async fn assign_resolves_remote_user_and_unassigns() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        let remote_user: i64 = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT remote_id FROM users WHERE remote_id IS NOT NULL ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let (code, body) = body_json(
            assign(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"userId": remote_user}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["message"], json!("Conversation assigned."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let assigned: Option<i64> = conn
            .query_row(
                "SELECT assignee_id FROM conversations WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(assigned.is_some(), "assignee resolved through the mirror");
        drop(conn);
        let (code, body) = body_json(
            assign(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"userId": null}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["message"], json!("Conversation unassigned."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let assigned: Option<i64> = conn
            .query_row(
                "SELECT assignee_id FROM conversations WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(assigned.is_none());
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'assignment_changed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audited, 2);
    }

    /// changeSubject: local row updated, audit written, reference message.
    #[tokio::test]
    async fn subject_updates_and_audits() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        let (code, body) = body_json(
            subject(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"subject": "Updated by pipeline"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["message"], json!("Subject updated."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let subject: String = conn
            .query_row(
                "SELECT subject FROM conversations WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(subject, "Updated by pipeline");
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'subject_changed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audited, 1);
    }

    /// setPriority: local write, no eval gate (the HS mapping defaults off),
    /// reference message + data, audit + activity event.
    #[tokio::test]
    async fn priority_local_write_reference_shape() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        // Eval mode ON must NOT block the local priority write.
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::settings::set_bool(&conn, "ai_evaluation_mode", true).unwrap();
        }
        let (code, body) = body_json(
            priority(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"priority": "high"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["message"], json!("Priority set to high."));
        assert_eq!(
            body["data"],
            json!({ "priority": "high", "hs_field_synced": false })
        );
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let p: String = conn
            .query_row(
                "SELECT supportos_priority FROM conversations WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(p, "high");
        let events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM activity_events WHERE event_type = 'priority_changed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(events, 1);
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'priority_changed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audited, 1);
    }

    /// set_state: stateId-based transition with history + activity + audit.
    #[tokio::test]
    async fn state_transition_by_id() {
        let state = make_pipeline_state().await;
        let (id, _remote) = first_conversation(&state);
        let state_row: (i64, String) = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT id, name FROM ticket_states ORDER BY sort_order LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        let (code, body) = body_json(
            set_state(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"stateId": state_row.0, "reason": "triaged"}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(
            body["message"],
            json!(format!("State set to {}.", state_row.1))
        );
        assert_eq!(body["previousStateId"], json!(null));
        // Idempotent same-state request.
        let (code, body) = body_json(
            set_state(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"stateId": state_row.0}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(
            body["message"],
            json!("Conversation already in this state.")
        );
        // Unknown state id.
        let (code, body) = body_json(
            set_state(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"stateId": 999_999}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["message"], json!("State not found."));
        // Clearing.
        let (code, body) = body_json(
            set_state(
                State(state.clone()),
                Path(id.to_string()),
                Some(Json(json!({"stateId": null}))),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body["message"], json!("State cleared."));
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        let transitions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM state_transitions WHERE conversation_id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(transitions, 2, "set + clear (the same-state one skipped)");
        let audited: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'ticket_state_changed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audited, 2);
    }
}

#[cfg(test)]
mod ops_tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
    use serde_json::json;

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

    /// ?ops=<unknown> -> 422 with the reference's exact message.
    #[tokio::test]
    async fn ops_unknown_key_is_422_with_the_reference_message() {
        let state = super::tests::make_state();
        let mut params = std::collections::HashMap::new();
        params.insert("ops".to_string(), "sla_at_risk".to_string());
        let (status, v) = body_of(list(State(state), Query(params)).await.into_response()).await;
        assert_eq!(status, 422, "{v}");
        assert_eq!(v["error"], json!("ValidationError"));
        assert_eq!(
            v["message"],
            json!("ops must be one of: unassigned, needs_first_response, customer_waiting, waiting_over_threshold, urgent, high_effort, repeated_issue, known_issue, ai_escalation.")
        );
    }

    /// ?ops=urgent applies the shared fragment, surfaces the note, and the
    /// list total equals the tile count on the demo DB (v1.7.0 invariant).
    #[tokio::test]
    async fn ops_urgent_applies_fragment_with_note() {
        let state = super::tests::make_pipeline_state().await;
        let snap = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::operations::snapshot(&conn, None).unwrap()
        };
        let tile_count = snap.count_of("urgent").unwrap_or(0);

        let mut params = std::collections::HashMap::new();
        params.insert("ops".to_string(), "urgent".to_string());
        let (status, v) = body_of(list(State(state), Query(params)).await.into_response()).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            v["notes"],
            json!(["Operations Center tile 'urgent' applied."])
        );
        assert_eq!(v["total"], json!(tile_count));
        // Every returned row is high/urgent priority.
        for c in v["conversations"].as_array().unwrap() {
            let p = c["priority"].as_str().unwrap_or("none");
            assert!(p == "high" || p == "urgent", "row priority: {p}");
        }
    }

    /// For EVERY tile key: drill-down total == tile count on the demo DB.
    #[tokio::test]
    async fn ops_drill_down_matches_tile_count_for_every_key() {
        let state = super::tests::make_pipeline_state().await;
        let snap = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::operations::snapshot(&conn, None).unwrap()
        };
        for key in crate::operations::OPS_TILE_WHITELIST {
            let expected = snap.count_of(key).unwrap_or(0);
            let mut params = std::collections::HashMap::new();
            params.insert("ops".to_string(), key.to_string());
            params.insert("pageSize".to_string(), "100".to_string());
            let (status, v) = body_of(
                list(State(state.clone()), Query(params))
                    .await
                    .into_response(),
            )
            .await;
            assert_eq!(status, 200, "{key}: {v}");
            assert_eq!(
                v["total"],
                json!(expected),
                "{key}: list total vs tile count"
            );
            let rows = v["conversations"].as_array().unwrap().len() as u64;
            assert!(rows <= 100, "{key}: page cap respected");
        }
    }

    /// The waiting threshold flows from the setting into the drill-down.
    #[tokio::test]
    async fn ops_waiting_threshold_uses_the_setting() {
        let state = super::tests::make_pipeline_state().await;
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::operations::set_waiting_threshold_minutes(&conn, 1).unwrap();
        }
        let snap = {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            crate::operations::snapshot(&conn, None).unwrap()
        };
        assert_eq!(snap.waiting_threshold_minutes, 1);
        let expected = snap.count_of("waiting_over_threshold").unwrap_or(0);
        let mut params = std::collections::HashMap::new();
        params.insert("ops".to_string(), "waiting_over_threshold".to_string());
        let (status, v) = body_of(list(State(state), Query(params)).await.into_response()).await;
        assert_eq!(status, 200);
        assert_eq!(v["total"], json!(expected));
    }
}
