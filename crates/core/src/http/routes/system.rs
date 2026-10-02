//! System routes — health, system info, onboarding, demo endpoints.
//!
//! Mirrors: src/server/routes/system.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /health — basic health check.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let db_ok = {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute_batch("SELECT 1").is_ok()
    };
    let status = if db_ok { "ok" } else { "error" };
    let code = if db_ok {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
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
///
/// Reference response shape includes subsystems: database, helpscout,
/// lmstudio, qdrant, sync, workers, in addition to status/version/time.
pub async fn health_detailed(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let db_ok = conn.execute_batch("SELECT 1").is_ok();

    // Run the self-check.
    let self_check = crate::self_check::run(&conn, None).ok();

    // Subsystem: Help Scout — configured when we have OAuth tokens.
    let hs_configured: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM oauth_tokens WHERE access_token IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let helpscout = json!({
        "configured": hs_configured > 0,
        "authenticated": hs_configured > 0,
    });

    // Subsystem: LM Studio / AI provider.
    let provider_kind = match crate::ai_center::get_ai_status(&conn) {
        Ok(s) => match s.provider_kind {
            crate::ai_center::ProviderKind::LmStudio => "lmstudio",
            crate::ai_center::ProviderKind::None => "none",
        },
        Err(_) => "none",
    };
    let lmstudio = json!({
        "connected": false,
        "provider": provider_kind,
        "models": [],
        "error": null,
    });

    // Subsystem: Qdrant — enabled behind the cargo feature.
    let qdrant_enabled = cfg!(feature = "qdrant");
    let qdrant = json!({
        "enabled": qdrant_enabled,
        "url": crate::settings::get_string(&conn, "qdrant_url").ok().flatten().unwrap_or_default(),
        "connected": false,
    });

    // Subsystem: Sync — running if any sync_run is in progress.
    let sync_running: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_runs WHERE status = 'running'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let webhook_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
        .unwrap_or(0);
    let sync = json!({
        "running": sync_running > 0,
        "is_realtime": webhook_count > 0,
        "last_sync": null,
    });

    // Subsystem: Workers — always running in the Tauri shell (background pollers).
    let workers = json!({
        "sync_interval_minutes": 5,
        "ratings_refresh_seconds": 30,
        "notification_sweep_seconds": 15,
        "customer_event_sweep_seconds": 60,
    });

    let status = if db_ok { "ok" } else { "error" };
    let code = if db_ok {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };

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
            "helpscout": helpscout,
            "lmstudio": lmstudio,
            "qdrant": qdrant,
            "sync": sync,
            "workers": workers,
        })),
    )
}

/// GET /api/system/db — database stats.
pub async fn db_stats(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let db_path = state.data_dir.join("supportos-plusplus.db");
    let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    (
        StatusCode::OK,
        Json(json!({
            "path": db_path.display().to_string(),
            "size_bytes": size,
            "tables": table_count,
            "wal": true,
            "migrations": crate::migrations::latest_version(),
        })),
    )
}

/// GET /api/system/capabilities — capability matrix.
pub async fn capabilities() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "matrix": [],
            "summary": {
                "implemented": 0,
                "total": 0,
                "tested": 0,
            }
        })),
    )
}

/// GET /api/system/tables — table stats.
pub async fn table_stats(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let tables: Vec<Value> =
        match conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name") {
            Ok(mut stmt) => match stmt.query_map([], |r| r.get::<_, String>(0)) {
                Ok(rows) => rows
                    .filter_map(|r| r.ok())
                    .map(|name| {
                        let count: i64 = conn
                            .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| {
                                r.get(0)
                            })
                            .unwrap_or(0);
                        json!({"name": name, "rows": count})
                    })
                    .collect(),
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };

    (StatusCode::OK, Json(json!({"tables": tables})))
}

/// GET /api/onboarding — onboarding state.
///
/// Reference response shape:
/// ```json
/// {
///   "step": "...", "completed": bool, "demo_mode": bool, "conversations": N,
///   "hs_configured": bool, "hs_authenticated": bool, "sync_state": "..."
/// }
/// ```
pub async fn onboarding(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let first_run = crate::settings::first_run_done(&conn).unwrap_or(false);
    let conv_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap_or(0);
    let hs_configured: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM oauth_tokens WHERE access_token IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let sync_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_runs", [], |r| r.get(0))
        .unwrap_or(0);
    let webhook_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
        .unwrap_or(0);
    let sync_state = if webhook_count > 0 {
        "receiving"
    } else if sync_count > 0 {
        "registered"
    } else {
        "new"
    };
    (
        StatusCode::OK,
        Json(json!({
            "step": if first_run { "complete" } else { "welcome" },
            "completed": first_run,
            "demo_mode": state.demo_mode,
            "conversations": conv_count,
            "hs_configured": hs_configured > 0,
            "hs_authenticated": hs_configured > 0,
            "sync_state": sync_state,
        })),
    )
}

/// POST /api/onboarding/step — set onboarding step.
pub async fn onboarding_step(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let step = body
        .get("step")
        .and_then(|v| v.as_str())
        .unwrap_or("welcome");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::settings::set_string(&conn, "onboarding_step", step);
    (StatusCode::OK, Json(json!({"ok": true})))
}

/// POST /api/onboarding/complete — mark onboarding done.
pub async fn onboarding_complete(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::settings::mark_first_run_done(&conn);
    (
        StatusCode::OK,
        Json(json!({"ok": true, "message": "Onboarding complete."})),
    )
}

/// POST /api/demo/enable — enable demo mode.
pub async fn demo_enable(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = crate::settings::set_bool(&conn, "demo_mode", true);
    let _ = crate::settings::mark_first_run_done(&conn);
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": "Demo mode active with a simulated Help Scout account. Run the initial sync from Sync Health to populate the demo database."
        })),
    )
}

/// POST /api/demo/simulate-incoming — simulate an incoming conversation (demo mode only).
pub async fn demo_simulate_incoming(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.demo_mode {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "message": "Not in demo mode."})),
        );
    }

    let subject = body
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("New question about exports");
    let text = body
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("Hello, can scheduled exports include the raw JSON fields in addition to CSV?");
    let mailbox_id = body
        .get("mailboxId")
        .and_then(|v| v.as_i64())
        .unwrap_or(201);
    let customer_id = body
        .get("customerRemoteId")
        .and_then(|v| v.as_i64())
        .unwrap_or(3003);

    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
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
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "message": "Failed to create conversation."})),
        );
    }
    // Fetch the inserted row's identity fields for the live event.
    let event_details = conn
        .query_row(
            "SELECT id, number, mailbox_id, subject FROM conversations ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0).ok().flatten(),
                    r.get::<_, Option<i64>>(1).ok().flatten(),
                    r.get::<_, Option<i64>>(2).ok().flatten(),
                    r.get::<_, Option<String>>(3).ok().flatten(),
                ))
            },
        )
        .ok();
    drop(conn);

    // Push a real-time `conversation-updated` event (reason 'sync') so
    // connected SSE clients refresh their inbox view immediately — the same
    // event the reference's sync worker emits after the sync job lands.
    if let Some((id, number, mailbox, subject)) = event_details {
        crate::http::event_bus::notify_conversation_updated(
            &state.bus,
            &crate::events::ConversationUpdatedEvent {
                conversation_id: id,
                conversation_number: number,
                mailbox_id: mailbox,
                subject,
                reason: "sync".into(),
                at: chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string(),
            },
        );
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": "Simulated incoming conversation. It will appear after refreshing."
        })),
    )
}

/// POST /api/demo/simulate-rating — simulate a CSAT rating (demo mode only).
pub async fn demo_simulate_rating(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.demo_mode {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "message": "Not in demo mode."})),
        );
    }

    let conversation_id = body.get("conversationRemoteId").and_then(|v| v.as_i64());
    let rating = body
        .get("rating")
        .and_then(|v| v.as_str())
        .unwrap_or("great");
    let _comments = body.get("comments").and_then(|v| v.as_str()).unwrap_or("");

    if conversation_id.is_none() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(
                json!({"error": "ValidationError", "message": "conversationRemoteId is required."}),
            ),
        );
    }

    let conv_id = conversation_id.unwrap();
    let rating_id = format!("demo_r_{}", chrono::Utc::now().timestamp_millis());
    let rating_num: u32 = match rating {
        "great" => 5,
        "okay" => 3,
        "bad" => 1,
        _ => 0,
    };

    // Push a RatingArrived event so the Reports / Customers pages refresh.
    // (The reference persists CSAT ratings to a `ratings` table; the port
    // receives them via the Help Scout API + ratings watcher, so demo mode
    // just emits the real-time event without persisting.)
    // The reference demo simulate-rating emits BOTH events (routes/system.ts:183-192):
    // rating-received with the full rating payload + ratings-refreshed (1 processed, 1 fresh).
    let rating_word = match rating {
        "great" => Some("great"),
        "okay" => Some("okay"),
        _ => Some("not-good"),
    };
    crate::http::event_bus::notify_rating_received(
        &state.bus,
        rating_word,
        Some(conv_id),
        None,
        None,
        None,
        None,
    );
    crate::http::event_bus::notify_ratings_refreshed(&state.bus, 1, 1);

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "message": format!("Simulated a {rating} rating on conversation #{conv_id}."),
            "rating_id": rating_id,
        })),
    )
}

/// POST /api/demo/simulate-webhook — simulate a webhook push (demo mode only).
///
/// Reference behavior: self-POSTs through the REAL HMAC-verified webhook
/// endpoint with a nonce to defeat dedup, so the actual pipeline
/// (verify → persist → process → enqueue) runs. The port reproduces this by
/// calling the same `process_webhook` pipeline directly with a computed
/// signature (network-loopback self-POST is an implementation detail; the
/// observable result — a processed webhook event + enqueued job — matches).
pub async fn demo_simulate_webhook(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.demo_mode {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "message": "Not in demo mode."})),
        );
    }

    let event = body
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("convo.created");
    let remote_id = body
        .get("conversationRemoteId")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Build the reference-shaped payload: `{conversationId, objectID, id,
    // nonce}` — the event type rides in the X-Helpscout-Event header, and the
    // nonce mirrors Help Scout's unique payloads so repeated demos are NOT
    // swallowed by dedup.
    let remote_id = if remote_id > 0 { remote_id } else { 1001 };
    let envelope = serde_json::json!({
        "conversationId": remote_id,
        "objectID": remote_id,
        "id": remote_id,
        "nonce": chrono::Utc::now().timestamp_millis(),
    });
    let envelope_bytes = envelope.to_string().into_bytes();

    let secret = {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        crate::settings::get_string(&conn, "webhook_secret")
            .ok()
            .flatten()
            .unwrap_or_else(|| "demo_webhook_secret".into())
    };
    let signature = crate::webhook::compute_signature(secret.as_bytes(), &envelope_bytes);

    let result = crate::webhook_handler::process_webhook(
        &state.conn.lock().unwrap_or_else(|p| p.into_inner()),
        secret.as_bytes(),
        &envelope_bytes,
        Some(&signature),
        event,
    );

    let event_id = format!("demo_wh_{}", chrono::Utc::now().timestamp_millis());
    match result {
        crate::webhook_handler::WebhookProcessResult::Accepted { row_id } => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "message": format!("{event} pushed through the webhook pipeline."),
                "remoteId": remote_id,
                "event_id": event_id,
            })),
        ),
        crate::webhook_handler::WebhookProcessResult::Duplicate { .. } => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "message": format!("{event} was a duplicate (already processed)."),
                "remoteId": remote_id,
                "event_id": event_id,
                "duplicate": true,
            })),
        ),
        crate::webhook_handler::WebhookProcessResult::SignatureInvalid => (
            StatusCode::OK,
            Json(json!({
                "ok": false,
                "message": "Signature verification failed for simulated webhook event.",
                "event_id": event_id,
            })),
        ),
        crate::webhook_handler::WebhookProcessResult::BadRequest => (
            StatusCode::OK,
            Json(json!({
                "ok": false,
                "message": "Simulated webhook payload was rejected.",
                "event_id": event_id,
            })),
        ),
    }
}
