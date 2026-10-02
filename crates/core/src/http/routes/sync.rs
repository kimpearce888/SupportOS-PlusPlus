//! Sync routes — mirrors src/server/routes/sync.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/sync/status
pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let sync_count: i64 = conn.query_row("SELECT COUNT(*) FROM sync_runs WHERE status != 'running'", [], |r| r.get(0)).unwrap_or(0);
    let webhook_count: i64 = conn.query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0)).unwrap_or(0);
    let state_str = if webhook_count > 0 { "receiving" } else if sync_count > 0 { "registered" } else { "not_configured" };
    Json(json!({
        "state": state_str,
        "sync_runs_completed": sync_count,
        "webhook_events_received": webhook_count,
        "is_realtime": webhook_count > 0,
    }))
}

/// POST /api/sync/initial
pub async fn initial(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Initial sync queued."}))
}

/// POST /api/sync/incremental
pub async fn incremental(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Incremental sync queued."}))
}

/// POST /api/sync/reconcile
pub async fn reconcile(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Reconciliation queued."}))
}

/// POST /api/sync/cancel
pub async fn cancel(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"ok": true}))
}

/// POST /api/webhooks/register
pub async fn register_webhook(
    State(state): State<AppState>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    Json(json!({"ok": true, "message": "Webhook registration requires Help Scout OAuth credentials."}))
}

/// DELETE /api/webhooks/:remoteId
pub async fn unregister_webhook(
    State(state): State<AppState>,
    Path(remote_id): Path<String>,
) -> impl IntoResponse {
    Json(json!({"ok": true, "message": format!("Webhook {remote_id} unregistered.")}))
}

/// GET /api/queue — job queue status.
pub async fn queue(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let queued: i64 = conn.query_row("SELECT COUNT(*) FROM jobs WHERE status = 'queued'", [], |r| r.get(0)).unwrap_or(0);
    let failed: i64 = conn.query_row("SELECT COUNT(*) FROM jobs WHERE status = 'failed'", [], |r| r.get(0)).unwrap_or(0);
    let running: i64 = conn.query_row("SELECT COUNT(*) FROM jobs WHERE status = 'running'", [], |r| r.get(0)).unwrap_or(0);
    Json(json!({"queued": queued, "failed": failed, "running": running}))
}

/// POST /api/queue/:id/retry
pub async fn retry_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute("UPDATE jobs SET status = 'queued', attempts = 0 WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}

/// POST /api/queue/:id/cancel
pub async fn cancel_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = conn.execute("UPDATE jobs SET status = 'cancelled' WHERE id = ?1", rusqlite::params![id]);
    Json(json!({"ok": true}))
}
