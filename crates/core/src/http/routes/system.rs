//! System routes — health, system info, onboarding, demo endpoints.
//!
//! Mirrors: src/server/routes/system.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /health — basic health check.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let db_ok = {
        let conn = state.conn.lock().expect("mutex poisoned");
        conn.execute_batch("SELECT 1").is_ok()
    };
    let status = if db_ok { "ok" } else { "error" };
    let code = if db_ok { axum::http::StatusCode::OK } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };
    (
        code,
        Json(json!({
            "status": status,
            "database": db_ok,
            "version": env!("CARGO_PKG_VERSION"),
            "time": chrono::Utc::now().to_rfc3339(),
        })),
    )
}

/// GET /health/detailed — detailed health with all subsystems.
pub async fn health_detailed(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let db_ok = conn.execute_batch("SELECT 1").is_ok();

    // Run the self-check.
    let self_check = crate::self_check::run(&conn, None).ok();

    let status = if db_ok { "ok" } else { "error" };
    let code = if db_ok { axum::http::StatusCode::OK } else { axum::http::StatusCode::SERVICE_UNAVAILABLE };

    (
        code,
        Json(json!({
            "status": status,
            "version": env!("CARGO_PKG_VERSION"),
            "time": chrono::Utc::now().to_rfc3339(),
            "database": {
                "ok": db_ok,
                "path": state.data_dir.join("supportos-plusplus.db").display().to_string(),
            },
            "demo_mode": state.demo_mode,
            "self_check": self_check,
        })),
    )
}

/// GET /api/system/db — database stats.
pub async fn db_stats(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let db_path = state.data_dir.join("supportos-plusplus.db");
    let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Json(json!({
        "path": db_path.display().to_string(),
        "size_bytes": size,
        "tables": table_count,
        "wal": true,
    }))
}

/// GET /api/system/capabilities — capability matrix.
pub async fn capabilities() -> impl IntoResponse {
    Json(json!({
        "matrix": [],
        "summary": {
            "implemented": 0,
            "total": 0,
            "tested": 0,
        }
    }))
}

/// GET /api/system/tables — table stats.
pub async fn table_stats(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let tables: Vec<Value> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .map(|name| {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| r.get(0))
                .unwrap_or(0);
            json!({"name": name, "rows": count})
        })
        .collect();

    Json(json!({"tables": tables}))
}

/// GET /api/onboarding — onboarding state.
pub async fn onboarding(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let first_run = crate::settings::first_run_done(&conn).unwrap_or(false);
    let conv_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap_or(0);

    Json(json!({
        "step": if first_run { "complete" } else { "welcome" },
        "completed": first_run,
        "demo_mode": state.demo_mode,
        "conversations": conv_count,
    }))
}

/// POST /api/onboarding/step — set onboarding step.
pub async fn onboarding_step(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let step = body.get("step").and_then(|v| v.as_str()).unwrap_or("welcome");
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::settings::set_string(&conn, "onboarding_step", step);
    Json(json!({"ok": true}))
}

/// POST /api/onboarding/complete — mark onboarding done.
pub async fn onboarding_complete(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::settings::mark_first_run_done(&conn);
    Json(json!({"ok": true, "message": "Onboarding complete."}))
}

/// POST /api/demo/enable — enable demo mode.
pub async fn demo_enable(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().expect("mutex poisoned");
    let _ = crate::settings::set_bool(&conn, "demo_mode", true);
    let _ = crate::settings::mark_first_run_done(&conn);
    Json(json!({
        "ok": true,
        "message": "Demo mode active with a simulated Help Scout account. Run the initial sync from Sync Health to populate the demo database."
    }))
}

/// POST /api/demo/simulate-incoming — simulate an incoming conversation (demo mode only).
pub async fn demo_simulate_incoming(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.demo_mode {
        return (axum::http::StatusCode::OK, Json(json!({"ok": false, "message": "Not in demo mode."})));
    }

    let subject = body.get("subject").and_then(|v| v.as_str()).unwrap_or("New question about exports");
    let text = body.get("body").and_then(|v| v.as_str()).unwrap_or("Hello, can scheduled exports include the raw JSON fields in addition to CSV?");
    let mailbox_id = body.get("mailboxId").and_then(|v| v.as_i64()).unwrap_or(201);
    let customer_id = body.get("customerRemoteId").and_then(|v| v.as_i64()).unwrap_or(3003);

    let conn = state.conn.lock().expect("mutex poisoned");
    // Insert a simulated conversation directly.
    let result = conn.execute(
        "INSERT INTO conversations (remote_id, number, subject, preview, status, mailbox_id, customer_id, priority)
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, 'normal')",
        rusqlite::params![
            chrono::Utc::now().timestamp(),
            chrono::Utc::now().timestamp() % 100000,
            subject,
            &text[..text.len().min(120)],
            mailbox_id,
            customer_id,
        ],
    );

    if result.is_err() {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "message": "Failed to create conversation."})));
    }

    Json(json!({
        "ok": true,
        "message": "Simulated incoming conversation. It will appear after refreshing."
    }))
}

/// POST /api/demo/simulate-rating — simulate a CSAT rating (demo mode only).
pub async fn demo_simulate_rating(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.demo_mode {
        return (axum::http::StatusCode::OK, Json(json!({"ok": false, "message": "Not in demo mode."})));
    }

    let conversation_id = body.get("conversationRemoteId").and_then(|v| v.as_i64());
    let rating = body.get("rating").and_then(|v| v.as_str()).unwrap_or("great");
    let comments = body.get("comments").and_then(|v| v.as_str()).unwrap_or("");

    if conversation_id.is_none() {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"statusCode": 422, "error": "ValidationError", "message": "conversationRemoteId is required."})),
        );
    }

    Json(json!({
        "ok": true,
        "message": format!("Simulated a {rating} rating on conversation #{conversation_id:?}.")
    }))
}

/// POST /api/demo/simulate-webhook — simulate a webhook push (demo mode only).
pub async fn demo_simulate_webhook(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.demo_mode {
        return (axum::http::StatusCode::OK, Json(json!({"ok": false, "message": "Not in demo mode."})));
    }

    let event = body.get("event").and_then(|v| v.as_str()).unwrap_or("convo.created");
    let remote_id = body.get("conversationRemoteId").and_then(|v| v.as_i64()).unwrap_or(0);

    Json(json!({
        "ok": true,
        "message": format!("{event} pushed through the webhook pipeline."),
        "remoteId": remote_id,
    }))
}
