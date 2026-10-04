//! Quality routes — mirrors src/server/routes/quality.ts
//!
//! M5 quality routes (v2.1.0, plan phases 26, 27, 29): knowledge gap
//! candidates, post-resolution QA, conversation friction. All bodies are
//! zod-validated (422 on hostile input); AI-dependent endpoints report
//! honest 503s when AI is disabled; every response carries its own honesty
//! notes - the data explains itself. GET endpoints never trigger a rebuild
//! (v2.2.1 audit fix: rebuilds go through the POST siblings only).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::super::server::AppState;

/// `Number(id)` must be a positive integer else 422 (reference pattern).
fn positive_int_422(field: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "statusCode": 422,
            "error": "ValidationError",
            "message": format!("{field} must be a positive integer.")
        })),
    )
        .into_response()
}

/// Parse a path segment the way the reference's `Number()` + integer check
/// does: finite numbers with no fractional part and > 0 pass.
fn path_positive_int(raw: &str, field: &str) -> Result<i64, Response> {
    match crate::conversation_ops::js_number(raw) {
        Some(v) if v.fract() == 0.0 && v > 0.0 && v <= i64::MAX as f64 => Ok(v as i64),
        _ => Err(positive_int_422(field)),
    }
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": message
        })),
    )
        .into_response()
}

fn service_503(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "statusCode": 503,
            "error": "ServiceUnavailable",
            "message": message
        })),
    )
        .into_response()
}

/// `clampDaysParam` (reference routes/helpers.ts): Number(value), NaN/garbage
/// falls back to the default, then clamps [min, max] after truncation.
fn clamp_days_param(raw: Option<&String>, fallback: i64, min: i64, max: i64) -> i64 {
    let Some(s) = raw.filter(|s| !s.is_empty()) else {
        return fallback;
    };
    match crate::conversation_ops::js_number(s) {
        Some(n) if n.is_finite() => (n.trunc() as i64).clamp(min, max),
        _ => fallback,
    }
}

// ---------------- Phase 26: knowledge gap engine ----------------

/// GET /api/knowledge/gaps — the grouped gap report. v2.2.1 audit fix: GET
/// no longer triggers a synchronous rebuild (?rebuild=1 made heavy SQLite
/// work fireable cross-site via <img> tags since GETs are unmetered).
pub async fn list_gaps(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    Json(crate::quality::gap_report(&conn))
}

/// POST /api/knowledge/gaps/rebuild — z.object({ days: int 1..3650 optional }).
pub async fn rebuild_gaps(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let days: i64 = match body.get("days") {
        None | Some(Value::Null) => 90,
        Some(v) => {
            let Some(n) = v.as_i64() else {
                return crate::conversation_ops::zod_422(
                    "days",
                    "Expected number, received non-number",
                );
            };
            if !(1..=3650).contains(&n) {
                return crate::conversation_ops::zod_422(
                    "days",
                    "Number must be greater than or equal to 1 and less than or equal to 3650",
                );
            }
            n
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let (candidates, new) = crate::quality::rebuild_gaps(&conn, days);
    Json(json!({ "candidates": candidates, "new": new })).into_response()
}

/// GET /api/knowledge/gaps/candidates/:id/draft — a suggested title +
/// outline for a human writer. Nothing is created or published.
pub async fn draft_candidate(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Ok(id) = path_positive_int(&id, "Candidate id") else {
        return positive_int_422("Candidate id");
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::quality::draft_gap(&conn, id) {
        Some(draft) => (StatusCode::OK, Json(draft)).into_response(),
        None => not_found("Knowledge candidate not found."),
    }
}

/// POST /api/knowledge/gaps/candidates/:id/decide — the human decision.
/// Deciding twice is a 409: a rebuild never resets human decisions.
pub async fn decide_candidate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Ok(id) = path_positive_int(&id, "Candidate id") else {
        return positive_int_422("Candidate id");
    };
    // z.object({ decision: enum, note: max(500) nullable optional }).parse
    // → the reference's global Zod handler answers 422 with the first issue.
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let decision = match body.get("decision") {
        Some(Value::String(s)) if s == "approved" || s == "rejected" => s.clone(),
        Some(Value::String(s)) => {
            return crate::conversation_ops::zod_422(
                "decision",
                &crate::conversation_ops::zod_enum_message(&["approved", "rejected"], s),
            );
        }
        Some(_) => {
            return crate::conversation_ops::zod_422(
                "decision",
                "Expected string, received non-string",
            );
        }
        None => return crate::conversation_ops::zod_422("decision", "Required"),
    };
    let note: Option<String> = match body.get("note") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) => {
            if n.chars().count() > 500 {
                return crate::conversation_ops::zod_422(
                    "note",
                    "String must contain at most 500 character(s)",
                );
            }
            Some(n.clone())
        }
        Some(_) => {
            return crate::conversation_ops::zod_422(
                "note",
                "Expected string, received non-string",
            );
        }
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // KnowledgeGapService.decide: UPDATE ... WHERE id = ? AND status is the
    // undecided state; 0 changes ⇒ not found or already decided ⇒ 409.
    match crate::quality::decide_gap(&conn, id, &decision, note.as_deref(), None) {
        Some(candidate) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "candidate": candidate })),
        )
            .into_response(),
        None => (
            StatusCode::CONFLICT,
            Json(json!({
                "statusCode": 409,
                "error": "Conflict",
                "message": "Candidate not found or already decided. Rebuild does not reset human decisions."
            })),
        )
            .into_response(),
    }
}

// ---------------- Phase 27: post-resolution QA ----------------

/// GET /api/qa/overview — the coverage snapshot.
pub async fn qa_overview(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    Json(crate::quality::qa_overview(&conn))
}

/// POST /api/qa/rebuild — the deterministic layer over closed conversations.
pub async fn qa_rebuild(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let conversations = crate::quality::rebuild_qa(&conn);
    Json(json!({ "conversations": conversations }))
}

/// Whether a conversation exists (non-deleted) — the routes' 404 guard.
fn conversation_exists(conn: &rusqlite::Connection, id: i64) -> bool {
    conn.query_row(
        "SELECT 1 FROM conversations WHERE id = ?1 AND deleted_at IS NULL",
        rusqlite::params![id],
        |_| Ok(()),
    )
    .is_ok()
}

/// GET /api/qa/:conversationId — { qa, friction } (the QA row is computed
/// lazily when absent; friction is analyzed live, exactly like the reference).
pub async fn qa_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> Response {
    let Ok(id) = path_positive_int(&conversation_id, "Conversation id") else {
        return positive_int_422("Conversation id");
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if !conversation_exists(&conn, id) {
        return not_found("Conversation not found.");
    }
    let ai_available =
        crate::settings::get_bool(&conn, "ai_enabled", true).unwrap_or(true);
    let qa = crate::quality::get_qa(&conn, id, ai_available);
    let friction = crate::quality::analyze_friction(&conn, id);
    Json(json!({ "qa": qa, "friction": friction })).into_response()
}

/// POST /api/qa/:conversationId/analyze — recompute; `includeAi: true` adds
/// the optional local-model layer (honest 503s when AI is disabled or the
/// model fails; the deterministic layer is still computed and stored).
pub async fn qa_analyze(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let Ok(id) = path_positive_int(&conversation_id, "Conversation id") else {
        return positive_int_422("Conversation id");
    };
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let include_ai = match body.get("includeAi") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return crate::conversation_ops::zod_422(
                "includeAi",
                "Expected boolean, received non-boolean",
            );
        }
    };
    {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        if !conversation_exists(&conn, id) {
            return not_found("Conversation not found.");
        }
        if include_ai {
            let ai_enabled =
                crate::settings::get_bool(&conn, "ai_enabled", true).unwrap_or(true);
            if !ai_enabled {
                return service_503(
                    "AI is disabled in Settings. The deterministic QA layer works without it; enable LM Studio for the optional AI layer.",
                );
            }
        }
    }
    if include_ai {
        let result = state
            .run_ai(move |conn| {
                Box::pin(async move {
                    crate::quality::ensure_quality_tables(conn).ok();
                    let backend = crate::ai_pipeline::backend_from_settings(conn);
                    crate::quality::compute_qa_ai_layer(conn, &backend, id).await
                })
            })
            .await;
        match result {
            Ok(outcome) => {
                if let Some(error) = outcome.error {
                    // The deterministic layer WAS computed and stored; the
                    // AI layer honestly did not run. Signal it clearly.
                    return service_503(&format!(
                        "{error} The deterministic QA layer was computed and stored."
                    ));
                }
                (
                    StatusCode::OK,
                    Json(json!({
                        "ok": true,
                        "qa": outcome.qa,
                        "ai": outcome.ai,
                        "error": Value::Null
                    })),
                )
                    .into_response()
            }
            Err(join) => service_503(&join),
        }
    } else {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        match crate::quality::compute_qa_deterministic(&conn, id) {
            None => not_found("Conversation not found."),
            Some(_) => {
                let ai_available =
                    crate::settings::get_bool(&conn, "ai_enabled", true).unwrap_or(true);
                let qa = crate::quality::get_qa(&conn, id, ai_available);
                (
                    StatusCode::OK,
                    Json(json!({
                        "ok": true,
                        "qa": qa,
                        "ai": Value::Null,
                        "error": Value::Null
                    })),
                )
                    .into_response()
            }
        }
    }
}

// ---------------- Phase 29: conversation friction ----------------

/// GET /api/friction/overview?days=30 — v2.2.1 audit fix: rebuild via POST
/// only, never on GET.
pub async fn friction_overview(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let days = clamp_days_param(params.get("days"), 30, 1, 3650);
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    Json(crate::quality::friction_overview(&conn, days))
}

/// POST /api/friction/rebuild — findings for every conversation with a
/// customer message (idempotent).
pub async fn friction_rebuild(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    crate::quality::ensure_quality_tables(&conn).ok();
    let (conversations, findings) = crate::quality::rebuild_friction(&conn);
    Json(json!({ "conversations": conversations, "findings": findings }))
}

/// GET /api/friction/:conversationId — per-conversation friction findings
/// (`{ findings: friction.analyzeConversation(id) }`).
pub async fn friction_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> Response {
    let Ok(id) = path_positive_int(&conversation_id, "Conversation id") else {
        return positive_int_422("Conversation id");
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if !conversation_exists(&conn, id) {
        return not_found("Conversation not found.");
    }
    let findings = crate::quality::analyze_friction(&conn, id);
    Json(json!({ "findings": findings })).into_response()
}
