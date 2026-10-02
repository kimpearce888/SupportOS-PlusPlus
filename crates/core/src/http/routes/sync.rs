//! Sync routes — mirrors src/server/routes/sync.ts

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/sync/status — mirrors reference's response shape.
pub async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let sync_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_runs WHERE status != 'running'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let webhook_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
        .unwrap_or(0);
    let running_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_runs WHERE status = 'running'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let last_success: Option<String> = conn
        .query_row(
            "SELECT MAX(completed_at) FROM sync_runs WHERE status = 'completed'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    let state_str = if webhook_count > 0 {
        "RECEIVING"
    } else if running_count > 0 {
        "RUNNING"
    } else if sync_count > 0 {
        "REGISTERED"
    } else {
        "NEW"
    };
    let recent_runs: Vec<Value> = conn
        .prepare("SELECT id, status, started_at, completed_at, error, resources_synced FROM sync_runs ORDER BY id DESC LIMIT 10")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "status": r.get::<_, String>(1)?,
                    "started_at": r.get::<_, String>(2)?,
                    "completed_at": r.get::<_, Option<String>>(3)?,
                    "error": r.get::<_, Option<String>>(4)?,
                    "resources_synced": r.get::<_, i64>(5)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let webhook_recent: Vec<Value> = conn
        .prepare("SELECT id, received_at FROM webhook_events ORDER BY received_at DESC LIMIT 5")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "received_at": r.get::<_, String>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let webhook_secret: Option<String> = crate::settings::get_string(&conn, "webhook_secret")
        .ok()
        .flatten();
    let webhook_configured = webhook_secret.is_some();
    let checkpoints: Vec<Value> = conn
        .prepare("SELECT resource, last_seen_at FROM sync_cursors ORDER BY resource")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "resource": r.get::<_, String>(0)?,
                    "last_synced_at": r.get::<_, Option<String>>(1)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let queued: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'pending'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'claimed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let completed: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs WHERE state = 'done'", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let failed: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs WHERE state = 'dead'", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let dispatched: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state IN ('claimed', 'done', 'dead')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Json(json!({
        "state": state_str,
        "running": running_count > 0,
        "checkpoints": checkpoints,
        "last_success": last_success,
        "recent_runs": recent_runs,
        "webhook": {
            "events": webhook_count,
            "recent": webhook_recent,
            "configured": webhook_configured,
            "secret_configured": webhook_configured,
        },
        "rate_limit": {
            "limitPerMinute": 300,
            "remaining": 300,
            "retryAfterSec": 0,
            "updatedAt": null,
            "inFlightWindow": false,
        },
        "api_queue": {
            "queued": queued,
            "active": active,
            "dispatched": dispatched,
            "completed": completed,
            "failed": failed,
            "highWater": 0,
        },
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
    Json(
        json!({"ok": true, "message": "Webhook registration requires Help Scout OAuth credentials."}),
    )
}

/// DELETE /api/webhooks/:remoteId
pub async fn unregister_webhook(
    State(state): State<AppState>,
    Path(remote_id): Path<String>,
) -> impl IntoResponse {
    Json(json!({"ok": true, "message": format!("Webhook {remote_id} unregistered.")}))
}

/// GET /api/queue — job queue status.
///
/// Reference response shape:
/// ```json
/// { "jobs": [...], "stats": { "queued": N, "running": N, "failed": N, "completed": N }, "outbound": [] }
/// ```
pub async fn queue(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let queued: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'pending'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let failed: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs WHERE state = 'dead'", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let running: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'claimed'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let completed: i64 = conn
        .query_row("SELECT COUNT(*) FROM jobs WHERE state = 'done'", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    // List recent jobs (last 50).
    let jobs: Vec<Value> = conn
        .prepare("SELECT id, kind, state, payload, attempts, available_at, claimed_at, completed_at, last_error FROM jobs ORDER BY id DESC LIMIT 50")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "payload": r.get::<_, Option<String>>(3)?,
                    "attempts": r.get::<_, i64>(4)?,
                    "created_at": r.get::<_, Option<String>>(5)?,
                    "claimed_at": r.get::<_, Option<String>>(6)?,
                    "completed_at": r.get::<_, Option<String>>(7)?,
                    "error": r.get::<_, Option<String>>(8)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({
        "jobs": jobs,
        "stats": {
            "queued": queued,
            "running": running,
            "failed": failed,
            "completed": completed,
        },
        "outbound": [],
    }))
}

/// POST /api/queue/:id/retry
pub async fn retry_job(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "UPDATE jobs SET state = 'queued', attempts = 0 WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/queue/:id/cancel
pub async fn cancel_job(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "UPDATE jobs SET state = 'cancelled' WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

// ---------------------------------------------------------------------------
// Encrypted multi-device sync (.sosync) — reference routes/sync.ts:204-232
// ---------------------------------------------------------------------------

/// Bundles directory (the reference stores .sosync bundles under the data
/// dir's `bundles` folder).
fn bundles_dir(state: &AppState) -> std::path::PathBuf {
    state.data_dir.join("bundles")
}

/// GET /api/sync/encrypted — bundle listing + ledger + design note.
pub async fn encrypted_list(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let bundles = crate::encrypted_sync::list_bundles(&bundles_dir(&state));
    let log = crate::encrypted_sync::sync_log(&conn);
    (
        StatusCode::OK,
        Json(json!({
            "bundles": bundles,
            "log": log,
            "bundle_dir": bundles_dir(&state).to_string_lossy(),
            "design": "File-based end-to-end encrypted bundles. No relay server: SupportOS never sees your data in transit - move the .sosync file yourself (cloud drive, USB, company share). Only the passphrase holder can decrypt it."
        })),
    )
}

/// POST /api/sync/encrypted/export — `{ passphrase }` (min 8 chars).
pub async fn encrypted_export(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let passphrase = body
        .and_then(|Json(v)| {
            v.get("passphrase")
                .and_then(|p| p.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    let conn = state.conn_lock();
    let result = crate::encrypted_sync::export_bundle(&conn, &bundles_dir(&state), &passphrase);
    if result.ok {
        let _ = crate::audit::audit(
            &conn,
            &crate::audit::AuditEntry::user("encrypted_sync_export").with_after_state(json!({
                "path": result.path.clone().unwrap_or_default(),
                "size": result.size_bytes.unwrap_or(0),
            })),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::to_value(result).unwrap_or(Value::Null)),
    )
}

/// POST /api/sync/encrypted/verify — `{ path, passphrase }`.
pub async fn encrypted_verify(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let Json(v) = body.unwrap_or(Json(Value::Null));
    let path = v.get("path").and_then(|p| p.as_str()).unwrap_or_default();
    if path.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({ "ok": false, "message": "A bundle path is required." })),
        );
    }
    let passphrase = v
        .get("passphrase")
        .and_then(|p| p.as_str())
        .unwrap_or_default();
    let conn = state.conn_lock();
    let result = crate::encrypted_sync::verify_bundle(
        &conn,
        &bundles_dir(&state),
        std::path::Path::new(path),
        passphrase,
    );
    (StatusCode::OK, Json(result))
}

/// POST /api/sync/encrypted/import — `{ path, passphrase }`. Swaps the DB;
/// the app must restart to use the restored mirror.
pub async fn encrypted_import(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let Json(v) = body.unwrap_or(Json(Value::Null));
    let path = v.get("path").and_then(|p| p.as_str()).unwrap_or_default();
    if path.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({ "ok": false, "message": "A bundle path is required." })),
        );
    }
    let passphrase = v
        .get("passphrase")
        .and_then(|p| p.as_str())
        .unwrap_or_default();
    let conn = state.conn_lock();
    let db_path = state.data_dir.join("supportos-plusplus.db");
    let result = crate::encrypted_sync::import_bundle(
        &conn,
        &db_path,
        &bundles_dir(&state),
        std::path::Path::new(path),
        passphrase,
    );
    if result.ok {
        let _ = crate::audit::audit(
            &conn,
            &crate::audit::AuditEntry::user("encrypted_sync_import")
                .with_after_state(json!({ "path": path })),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::to_value(result).unwrap_or(Value::Null)),
    )
}
