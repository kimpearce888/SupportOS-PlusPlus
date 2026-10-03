//! Interactions routes — mirrors src/server/routes/interactions.ts
//!
//! Interaction signals — AI-derived signals about a customer's
//! engagement (response times, sentiment, escalation risk). Used by
//! the customer support health score. Stored in `interaction_signals`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/interaction/:conversationId — list signals for a conversation.
///
/// Returns 404 when the conversation does not exist (matching the reference).
pub async fn get(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM conversations WHERE remote_id = ?1",
            rusqlite::params![conversation_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Conversation not found."
            })),
        );
    }
    let signals: Vec<Value> = conn
        .prepare("SELECT id, conversation_id, signal_type, signal_value, confidence, created_at FROM interaction_signals WHERE conversation_id = ?1 ORDER BY created_at DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![conversation_id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, i64>(1)?,
                    "signal_type": r.get::<_, String>(2)?,
                    "signal_value": r.get::<_, String>(3)?,
                    "confidence": r.get::<_, f64>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({"conversationId": conversation_id, "signals": signals})),
    )
}

/// POST /api/interaction/:conversationId/refresh — recompute interaction signals.
pub async fn refresh(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    // Without an AI provider, this is a no-op + a real-time notification
    // so the UI shows a "refreshed" state.
    Json(
        json!({"ok": true, "conversationId": conversation_id, "message": "Interaction signals refresh queued."}),
    )
}

/// GET /api/interaction/:conversationId/evidence — evidence supporting the signals.
pub async fn evidence(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let evidence: Vec<Value> = conn
        .prepare("SELECT id, conversation_id, evidence_type, evidence_data, created_at FROM interaction_evidence WHERE conversation_id = ?1 ORDER BY created_at DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![conversation_id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "conversation_id": r.get::<_, i64>(1)?,
                    "evidence_type": r.get::<_, String>(2)?,
                    "evidence_data": r.get::<_, String>(3)?,
                    "created_at": r.get::<_, String>(4)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"conversationId": conversation_id, "evidence": evidence}))
}

/// GET /api/interaction/profile/:customerId — interaction profile for a customer.
///
/// Returns 404 when the customer does not exist (matching the reference).
pub async fn profile(
    State(state): State<AppState>,
    Path(customer_id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM customers WHERE remote_id = ?1",
            rusqlite::params![customer_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Customer not found or no interaction data."
            })),
        );
    }
    // Aggregate the customer's signals across all their conversations.
    let total_signals: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM interaction_signals s
             JOIN conversations c ON c.remote_id = s.conversation_id
             WHERE c.customer_id = ?1",
            rusqlite::params![customer_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let signal_breakdown: Vec<Value> = conn
        .prepare("SELECT s.signal_type, COUNT(*) FROM interaction_signals s JOIN conversations c ON c.remote_id = s.conversation_id WHERE c.customer_id = ?1 GROUP BY s.signal_type ORDER BY 2 DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(rusqlite::params![customer_id], |r| {
                Ok(json!({
                    "signal_type": r.get::<_, String>(0)?,
                    "count": r.get::<_, i64>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "customerId": customer_id,
            "total_signals": total_signals,
            "signal_breakdown": signal_breakdown,
        })),
    )
}

// ─── Human response-preference overrides (reference interactions.ts:93-126) ─

/// RESPONSE_PREFERENCE_VALUES (reference shared/constants.ts:101) — the
/// closed vocabulary the engine and draft pipeline consume.
const RESPONSE_PREFERENCE_VALUES: [&str; 6] = [
    "concise",
    "detailed",
    "step_by_step",
    "technical",
    "conversational",
    "outcome_focused",
];

/// The reference's customer lookup for the override routes:
/// `peopleRepo.getCustomerByLocalId(customerId)` — the LOCAL id (the port's
/// `customers::get_customer` uses the same key).
fn customer_exists_by_local_id(
    conn: &std::sync::MutexGuard<'_, rusqlite::Connection>,
    id: i64,
) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM customers WHERE id = ?1",
        rusqlite::params![id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// POST /api/interaction/profile/:customerId/override — human override
/// (spec #22, #45, #56): takes precedence over AI inference. Only the
/// response preference is overridable (reference interactions.ts:93-109).
pub async fn set_override(
    State(state): State<AppState>,
    Path(customer_id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    // Reference: `Number(params.customerId)` — a non-numeric id is NaN and
    // lands on the same 404 as an unknown customer.
    let Ok(customer_id) = customer_id.parse::<i64>() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Customer not found."
            })),
        )
            .into_response();
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if !customer_exists_by_local_id(&conn, customer_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Customer not found."
            })),
        )
            .into_response();
    }
    // interactionOverrideSchema: field literal 'response_preference', value
    // in the closed preference vocabulary, reason ≤ 500 nullable. The
    // reference returns ONE 422 message for every violation.
    let body = body.map(|b| b.0);
    let valid = match body.as_ref() {
        Some(b) => {
            let field_ok = b.get("field").and_then(Value::as_str) == Some("response_preference");
            let value_ok = b
                .get("value")
                .and_then(Value::as_str)
                .is_some_and(|v| RESPONSE_PREFERENCE_VALUES.contains(&v));
            let reason_ok = match b.get("reason") {
                None | Some(Value::Null) => true,
                Some(Value::String(r)) => r.chars().count() <= 500,
                Some(_) => false,
            };
            field_ok && value_ok && reason_ok
        }
        None => false,
    };
    if !valid {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "Invalid override payload: field must be response_preference and value must be a known preference (concise, detailed, step_by_step, technical, conversational, outcome_focused)."
            })),
        )
            .into_response();
    }
    let b = body.unwrap_or_default();
    let value = b.get("value").and_then(Value::as_str).unwrap_or_default();
    let reason = b.get("reason").and_then(Value::as_str);
    // Upsert the single override row per (customer, field).
    let _ = conn.execute(
        "INSERT INTO interaction_overrides (customer_id, field, value, reason)
         VALUES (?1, 'response_preference', ?2, ?3)
         ON CONFLICT (customer_id, field) DO UPDATE SET
            value = excluded.value,
            reason = excluded.reason,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        rusqlite::params![customer_id, value, reason],
    );
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry::user("interaction_override_set:response_preference")
            .with_after_state(json!({ "value": value })),
    );
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": "Human preference saved. It takes precedence over AI-inferred preferences."
        })),
    )
        .into_response()
}

/// DELETE /api/interaction/profile/:customerId/override/:field — clear the
/// override so AI-inferred observations apply again (reference
/// interactions.ts:112-126). A missing override row is a 404.
pub async fn clear_override(
    State(state): State<AppState>,
    Path((customer_id, field)): Path<(String, String)>,
) -> Response {
    let Ok(customer_id) = customer_id.parse::<i64>() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Customer not found."
            })),
        )
            .into_response();
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if !customer_exists_by_local_id(&conn, customer_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Customer not found."
            })),
        )
            .into_response();
    }
    if field != "response_preference" {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "Only response_preference overrides exist."
            })),
        )
            .into_response();
    }
    let removed = conn
        .execute(
            "DELETE FROM interaction_overrides
             WHERE customer_id = ?1 AND field = 'response_preference'",
            rusqlite::params![customer_id],
        )
        .unwrap_or(0)
        > 0;
    if !removed {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "No override set for this customer."
            })),
        )
            .into_response();
    }
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry::user("interaction_override_cleared:response_preference"),
    );
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": "Override removed. AI-inferred preferences (if any) apply again."
        })),
    )
        .into_response()
}
