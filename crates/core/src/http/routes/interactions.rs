//! Interactions routes — mirrors src/server/routes/interactions.ts
//!
//! Interaction signals — AI-derived signals about a customer's
//! engagement (response times, sentiment, escalation risk). Used by
//! the customer support health score. Stored in `interaction_signals`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
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
    crate::http::event_bus::notify_sync(&state.bus, "interactions", 1);
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
