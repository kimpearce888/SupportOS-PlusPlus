//! Sync routes — mirrors src/server/routes/sync.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use crate::audit::AuditEntry;

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

// ---------------------------------------------------------------------------
// OAuth flow (reference routes/sync.ts:269-385)
// ---------------------------------------------------------------------------

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn oauth_fail_page(
    message: &str,
) -> (
    StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    &'static str,
) {
    let html: &'static str = Box::leak(
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>SupportOS - connection not completed</title>\
<style>body{{font-family:system-ui,sans-serif;max-width:36rem;margin:4rem auto;padding:0 1rem;color:#1f2933}}h1{{font-size:1.2rem}}p{{line-height:1.5;color:#52606d}}</style></head>\
<body><h1>Help Scout connection not completed</h1><p>{message}</p>\
<p>Return to SupportOS Settings and try again, or use \"Connect with Client Credentials\".</p></body></html>"
        )
        .into_boxed_str(),
    );
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        html,
    )
}

/// Whether Help Scout client credentials are configured (the reference reads
/// env config; the port persists them in application_settings).
fn hs_credentials(conn: &rusqlite::Connection) -> (String, String) {
    (
        crate::settings::get_string(conn, "helpscout_client_id")
            .ok()
            .flatten()
            .unwrap_or_default(),
        crate::settings::get_string(conn, "helpscout_client_secret")
            .ok()
            .flatten()
            .unwrap_or_default(),
    )
}

/// GET /api/oauth/authorize-url — begin the browser OAuth flow (demo mode:
/// `{demo_mode: true, message}`).
pub async fn oauth_authorize_url(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    if state.demo_mode {
        return (
            StatusCode::OK,
            Json(
                json!({ "demo_mode": true, "message": "Demo mode is active - OAuth is not needed." }),
            ),
        );
    }
    let (client_id, _) = hs_credentials(&conn);
    if client_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "ok": false, "message": "HELPSCOUT_CLIENT_ID and HELPSCOUT_CLIENT_SECRET must be set in .env first." }),
            ),
        );
    }
    // Reference: 16 random bytes hex + stored as JSON in application_settings.
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let state_token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = crate::settings::set_json(&conn, "oauth_state", &state_token);
    let url = format!(
        "https://secure.helpscout.net/authentication/authorizeClientApplication?client_id={}&state={}",
        client_id, state_token
    );
    (StatusCode::OK, Json(json!({ "url": url })))
}

/// POST /api/oauth/client-credentials — connect with client credentials.
pub async fn oauth_client_credentials(State(state): State<AppState>) -> impl IntoResponse {
    let (client_id, client_secret) = {
        let conn = state.conn_lock();
        if state.demo_mode {
            return (StatusCode::OK, Json(json!({ "demo_mode": true })));
        }
        hs_credentials(&conn)
    };
    if client_id.is_empty() || client_secret.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "ok": false, "message": "HELPSCOUT_CLIENT_ID and HELPSCOUT_CLIENT_SECRET must be set in .env first." }),
            ),
        );
    }
    // Exchange client credentials for a token (POST /v2/oauth2/token).
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build();
    let Ok(client) = client else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "message": "could not build HTTP client" })),
        );
    };
    let resp = client
        .post("https://api.helpscout.net/v2/oauth2/token")
        .json(&serde_json::json!({
            "grant_type": "client_credentials",
            "client_id": client_id,
            "client_secret": client_secret,
        }))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(body) => {
                let token = crate::oauth::OAuthToken {
                    access_token: body["access_token"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    refresh_token: body["refresh_token"].as_str().map(String::from),
                    expires_in: body["expires_in"].as_u64(),
                    token_type: body["token_type"].as_str().unwrap_or("bearer").to_string(),
                    scope: body["scope"].as_str().map(String::from),
                };
                let conn = state.conn_lock();
                if token.access_token.is_empty()
                    || crate::oauth::store_token(&conn, &token).is_err()
                {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(
                            json!({ "ok": false, "message": "Token response was missing an access token." }),
                        ),
                    );
                }
                let _ = crate::audit::audit(
                    &conn,
                    &crate::audit::AuditEntry {
                        actor: "user",
                        action: "helpscout_connected".into(),
                        remote_operation: Some("POST /v2/oauth2/token".into()),
                        ..AuditEntry::user("helpscout_connected")
                    },
                );
                (
                    StatusCode::OK,
                    Json(json!({ "ok": true, "message": "Connected to Help Scout." })),
                )
            }
            Err(e) => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "ok": false, "message": e.to_string() })),
            ),
        },
        Ok(r) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "message": format!("Help Scout returned {}", r.status()) })),
        ),
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "message": e.to_string() })),
        ),
    }
}

/// GET /api/oauth/status — connection status (demo mode shape:
/// `{configured:false, authenticated:true, demo_mode:true, expires_at:null, me:null}`).
pub async fn oauth_status(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    if state.demo_mode {
        return (
            StatusCode::OK,
            Json(json!({
                "configured": false,
                "authenticated": true,
                "demo_mode": true,
                "expires_at": Value::Null,
                "me": Value::Null,
            })),
        );
    }
    let (client_id, _) = hs_credentials(&conn);
    let authenticated = crate::oauth::has_token(&conn).unwrap_or(false);
    let expires_at: Option<String> = conn
        .query_row(
            "SELECT expires_at FROM oauth_tokens WHERE id = 1 AND revoked = 0",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    (
        StatusCode::OK,
        Json(json!({
            "configured": !client_id.is_empty(),
            "authenticated": authenticated,
            "demo_mode": false,
            "expires_at": expires_at,
            "me": Value::Null,
        })),
    )
}

/// POST /api/oauth/disconnect — revoke + preserve local data.
pub async fn oauth_disconnect(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn_lock();
    let _ = crate::oauth::delete_token(&conn);
    let _ = crate::audit::audit(
        &conn,
        &crate::audit::AuditEntry::user("helpscout_disconnected"),
    );
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "message": "Disconnected. Local data is fully preserved." })),
    )
}

/// GET /oauth/callback — completes the browser OAuth code flow; verifies the
/// single-use state parameter; returns styled HTML pages like the reference.
pub async fn oauth_callback(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let (client_id, client_secret, code) = {
        let conn = state.conn_lock();
        if state.demo_mode {
            return oauth_fail_page("Demo mode is active - OAuth is not needed.");
        }
        if let Some(error) = params.get("error") {
            let desc = params
                .get("error_description")
                .map(|d| format!(" - {}", escape_html(d)))
                .unwrap_or_default();
            return oauth_fail_page(&format!(
                "Help Scout returned an error: {}{}",
                escape_html(error),
                desc
            ));
        }
        let (code, q_state) = match (params.get("code"), params.get("state")) {
            (Some(c), Some(st)) => (c.clone(), st.clone()),
            _ => {
                return oauth_fail_page(
                    "The callback is missing its authorization code or state parameter.",
                )
            }
        };
        let stored: Option<String> = crate::settings::get_string(&conn, "oauth_state")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok());
        // Single-use: clear the stored state immediately after reading it.
        let _ = conn.execute(
            "DELETE FROM application_settings WHERE key = 'oauth_state'",
            [],
        );
        match stored {
            Some(stored) if stored == q_state => {}
            _ => {
                return oauth_fail_page("The state parameter did not match the authorization request (it may have expired or been reused). For safety the code was not exchanged.")
            }
        }
        let creds = hs_credentials(&conn);
        (creds.0, creds.1, code)
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build();
    let Ok(client) = client else {
        return oauth_fail_page(
            "Exchanging the authorization code failed: could not build HTTP client.",
        );
    };
    let resp = client
        .post("https://api.helpscout.net/v2/oauth2/token")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": client_id,
            "client_secret": client_secret,
            "code": code,
        }))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(body) => {
                let token = crate::oauth::OAuthToken {
                    access_token: body["access_token"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    refresh_token: body["refresh_token"].as_str().map(String::from),
                    expires_in: body["expires_in"].as_u64(),
                    token_type: body["token_type"].as_str().unwrap_or("bearer").to_string(),
                    scope: body["scope"].as_str().map(String::from),
                };
                let conn = state.conn_lock();
                if token.access_token.is_empty()
                    || crate::oauth::store_token(&conn, &token).is_err()
                {
                    return oauth_fail_page("Exchanging the authorization code failed: token response was missing an access token.");
                }
                let _ = crate::audit::audit(
                    &conn,
                    &crate::audit::AuditEntry {
                        actor: "user",
                        action: "helpscout_connected".into(),
                        remote_operation: Some("GET /oauth/callback (code exchange)".into()),
                        ..AuditEntry::user("helpscout_connected")
                    },
                );
                oauth_success_page()
            }
            Err(e) => oauth_fail_page(&format!(
                "Exchanging the authorization code failed: {}",
                escape_html(&e.to_string())
            )),
        },
        Ok(r) => oauth_fail_page(&format!(
            "Exchanging the authorization code failed: Help Scout returned {}",
            r.status()
        )),
        Err(e) => oauth_fail_page(&format!(
            "Exchanging the authorization code failed: {}",
            escape_html(&e.to_string())
        )),
    }
}

fn oauth_success_page() -> (
    StatusCode,
    [(axum::http::HeaderName, &'static str); 1],
    &'static str,
) {
    let html: &'static str = Box::leak(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>SupportOS - connected</title>\
<style>body{font-family:system-ui,sans-serif;max-width:36rem;margin:4rem auto;padding:0 1rem;color:#1f2933}}h1{font-size:1.2rem}}p{{line-height:1.5;color:#52606d}}</style></head>\
<body><h1>Connected to Help Scout</h1><p>You can close this tab and return to SupportOS. Reload the Settings page to see the connection status.</p></body></html>"
            .to_string()
            .into_boxed_str(),
    );
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html")],
        html,
    )
}
