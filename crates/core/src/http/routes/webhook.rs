//! Webhook route — mirrors the reference `POST /api/webhooks/helpscout`
//! (`src/server/services/webhookEndpoint.ts` + `app.ts` wiring).
//!
//! Observable contract (verified against the reference source):
//! - No secret configured → accepted WITHOUT signature verification.
//! - Secret configured + invalid/missing signature → **401**
//!   `{"error":"Invalid webhook signature"}` (NOT persisted).
//! - Malformed JSON body → **400** Fastify envelope
//!   `{"statusCode":400,"error":"Bad Request","message":"Request body is not valid JSON."}`
//! - Duplicate (sha256(eventType:payload) hash) → **200**
//!   `{"received":true,"duplicate":true}`
//! - New event → processed asynchronously → **200** `{"received":true}`

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// POST /api/webhooks/helpscout — webhook receiver with HMAC-SHA1 verification.
pub async fn handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let raw_body = String::from_utf8_lossy(&body).into_owned();

    // The secret comes from settings (the reference reads it from config at
    // boot; the port persists it in `application_settings`).
    let secret = crate::settings::get_string(
        &state.conn.lock().unwrap_or_else(|p| p.into_inner()),
        "webhook_secret",
    )
    .ok()
    .flatten()
    .unwrap_or_default();

    let signature = headers
        .get("x-helpscout-signature")
        .and_then(|v| v.to_str().ok());

    // The reference reads the event type from the `X-Helpscout-Event` header
    // (never from the body); "unknown" when absent.
    let event_type = headers
        .get("x-helpscout-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let result = crate::webhook_handler::process_webhook(
        &state.conn.lock().unwrap_or_else(|p| p.into_inner()),
        secret.as_bytes(),
        raw_body.as_bytes(),
        signature,
        event_type,
    );

    match result {
        crate::webhook_handler::WebhookProcessResult::Accepted { row_id } => {
            // Notify SSE clients (the UI reacts to webhook pushes).
            crate::http::event_bus::notify_webhook(&state.bus, &row_id.to_string());
            (StatusCode::OK, Json(json!({"received": true})))
        }
        crate::webhook_handler::WebhookProcessResult::Duplicate { .. } => (
            StatusCode::OK,
            Json(json!({"received": true, "duplicate": true})),
        ),
        crate::webhook_handler::WebhookProcessResult::SignatureInvalid => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid webhook signature"})),
        ),
        crate::webhook_handler::WebhookProcessResult::BadRequest => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "statusCode": 400,
                "error": "Bad Request",
                "message": "Request body is not valid JSON."
            })),
        ),
    }
}

/// Marker so `Value` stays imported for future payload handling.
#[allow(dead_code)]
fn _unused(v: Value) {}
