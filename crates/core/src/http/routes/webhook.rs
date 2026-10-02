//! Webhook route — mirrors src/server/services/webhookEndpoint.ts

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;
/// POST /api/webhooks/helpscout — webhook receiver with HMAC-SHA1 verification.
pub async fn handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let raw_body = String::from_utf8_lossy(&body);

    // Verify HMAC-SHA1 signature.
    let secret = crate::settings::get_string(
        &state.conn.lock().expect("mutex poisoned"),
        "webhook_secret",
    )
    .ok()
    .flatten()
    .unwrap_or_default();

    if !secret.is_empty() {
        let signature = headers
            .get("x-helpscout-signature")
            .and_then(|v| v.to_str().ok());
        if let Some(sig) = signature {
            if crate::webhook::verify_signature(secret.as_bytes(), raw_body.as_bytes(), sig)
                .is_err()
            {
                return Json(
                    json!({"_status": 401, "received": false, "error": "Invalid signature."}),
                );
            }
        }
    }

    // Parse the webhook payload.
    let payload: Value = match serde_json::from_str(&raw_body) {
        Ok(v) => v,
        Err(e) => {
            return Json(
                json!({"_status": 400, "received": false, "error": format!("Invalid JSON: {e}")}),
            );
        }
    };

    // Persist the webhook event (dedup by hash).
    let event_type = headers
        .get("x-helpscout-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");
    let event_hash = format!("{:x}", crc32fast::hash(raw_body.as_bytes()));

    let conn = state.conn.lock().expect("mutex poisoned");
    let result = conn.execute(
        "INSERT OR IGNORE INTO webhook_events (id, event_type, payload, received_at) VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![event_hash, event_type, raw_body],
    );

    if result.is_err() {
        return Json(
            json!({"_status": 500, "received": false, "error": "Failed to persist webhook event."}),
        );
    }

    let duplicate = conn.changes() == 0;
    if duplicate {
        return Json(json!({"received": true, "duplicate": true}));
    }

    // Acknowledge fast — sync job runs asynchronously.
    // Emit a `WebhookReceived` event so any connected SSE clients
    // (browser tabs watching the Sync Health page) refresh in real time.
    let event_id = event_hash.clone();
    drop(conn); // release the mutex before emitting
    crate::http::event_bus::notify_webhook(&state.bus, &event_id);

    Json(
        json!({"received": true, "duplicate": false, "event_type": event_type, "event_id": event_id}),
    )
}
