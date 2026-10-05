//! Operations routes — mirrors src/server/routes/operations.ts

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;
use axum::response::IntoResponse;

/// GET /api/operations/center — operations center snapshot.
///
/// Scope: `?mailboxes=1,2` (local ids, comma list, max 50 — duplicate params
/// arrive as repeated keys and are NOT supported, matching the reference's
/// array-join normalization); omitted/empty/"all" = all inboxes.
///
/// Reference response shape:
/// ```json
/// { "generated_at": "...", "mailbox_scope": null, "tiles": [
///     { "key": "unassigned", "label": "Unassigned", "count": 0,
///       "severity": "info", "drill": {"type":"inbox","params":{...}}, "note": null }
/// ], "waiting_threshold_minutes": 240 }
/// ```
pub async fn center(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // Scope: ?mailboxes=1,2 (local ids); omitted/empty/"all" = all inboxes.
    let mailbox_ids: Option<Vec<i64>> = params
        .get("mailboxes")
        .filter(|raw| !raw.is_empty() && *raw != "all")
        .map(|raw| {
            raw.split(',')
                .filter_map(|v| v.trim().parse::<i64>().ok())
                .filter(|v| *v > 0)
                .take(50)
                .collect::<Vec<_>>()
        });
    match crate::operations::snapshot(&conn, mailbox_ids.as_deref()) {
        Ok(snap) => (
            StatusCode::OK,
            Json(serde_json::to_value(&snap).unwrap_or_else(|_| json!({}))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string(),
            })),
        ),
    }
}

/// GET /api/operations/workload — the real v1.8.0 workload snapshot
/// (reference `ctx.workload.snapshot()`): per-agent open/pending/waiting/
/// urgent/SLA-risk counts, the weighted pressure load against explicit
/// capacity, availability from the synced user statuses, per-team rollups
/// and the honest method notes. The v1.x route answered hardcoded zeros.
pub async fn workload(State(state): State<AppState>) -> impl IntoResponse {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::workload::snapshot(&conn) {
        Ok(snap) => (
            StatusCode::OK,
            Json(serde_json::to_value(&snap).unwrap_or_else(|_| json!({}))),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string(),
            })),
        ),
    }
}

/// PUT /api/operations/capacity — reference `setCapacityModel`:
/// `CapacityModelUpdateSchema` with a 422 carrying the FIRST issue as the
/// message and every issue in `detail`, then
/// `{ saved: true, capacity_model }`.
pub async fn set_capacity(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    match crate::workload::validate_capacity_model(&body) {
        Err(issues) => {
            let message = issues
                .first()
                .cloned()
                .unwrap_or_else(|| "Invalid capacity model.".to_string());
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "statusCode": 422,
                    "error": "ValidationError",
                    "message": message,
                    "detail": issues,
                })),
            )
        }
        Ok(model) => {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            match crate::workload::set_capacity_model(&conn, &model) {
                Ok(()) => {
                    let stored = crate::workload::get_capacity_model(&conn);
                    (
                        StatusCode::OK,
                        Json(json!({
                            "saved": true,
                            "capacity_model": serde_json::to_value(&stored)
                                .unwrap_or_else(|_| json!({})),
                        })),
                    )
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "statusCode": 500,
                        "error": "InternalError",
                        "message": e,
                    })),
                ),
            }
        }
    }
}

/// PUT /api/operations/waiting-threshold
///
/// Reference `setWaitingThresholdMinutes`: clamped to 1..=20160.
pub async fn set_waiting_threshold(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let Some(minutes) = body.get("minutes").and_then(|v| v.as_i64()) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "statusCode": 422,
                "error": "ValidationError",
                "message": "minutes must be an integer.",
            })),
        );
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::operations::set_waiting_threshold_minutes(&conn, minutes) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string(),
            })),
        ),
    }
}

/// GET /api/operations/suggested-assignees — the real recommendation read
/// (reference `ctx.workload.suggestedAssignees(limit)`): the top unassigned
/// conversations (urgent first, then longest-waiting) with a read-only
/// suggested assignee each, the reasoning exposed. Nothing reassigns
/// automatically. `?limit=` clamps to 1..=50 (default 10).
pub async fn suggested_assignees(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params.get("limit");
    let limit = match raw.filter(|s| !s.is_empty()) {
        Some(s) => match crate::conversation_ops::js_number(s) {
            Some(n) if n.is_finite() => (n.trunc() as i64).clamp(1, 50) as u32,
            _ => 10,
        },
        None => 10,
    };
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::workload::suggested_assignees(&conn, limit) {
        Ok(suggestions) => (
            StatusCode::OK,
            Json(json!({
                "suggestions": serde_json::to_value(&suggestions)
                    .unwrap_or_else(|_| json!([])),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "statusCode": 500,
                "error": "InternalError",
                "message": e.to_string(),
            })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn make_state() -> AppState {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::bootstrap::apply_all(&mut conn).unwrap();
        AppState {
            conn: Arc::new(Mutex::new(conn)),
            data_dir: std::path::PathBuf::from("/tmp"),
            port: 3000,
            host: "127.0.0.1".into(),
            demo_mode: false,
            bus: crate::http::EventBus::new(64),
            limiter: crate::http::RateLimiter::new(),
            sync: None,
            real: None,
            provider_kind: "fake".into(),
            workers: None,
            qdrant: Arc::new(crate::vectorstore_qdrant::EmbeddedQdrant::new(
                "/tmp/spp-test-qdrant",
                "http://127.0.0.1:6333",
                false,
            )),
        }
    }

    async fn body_json(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    fn seed_agent_with_open(state: &AppState, remote_id: i64, name: &str, opens: i64) -> i64 {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO users (remote_id, first_name, last_name) VALUES (?1, ?2, 'Agent')",
            rusqlite::params![remote_id, name],
        )
        .unwrap();
        let uid = conn.last_insert_rowid();
        for i in 0..opens {
            conn.execute(
                "INSERT INTO conversations
                    (remote_id, number, status, mailbox_id, customer_id, assignee_id, created_at)
                 VALUES (?1, ?1, 'active', 1, 3001, ?2, '2026-10-01T00:00:00Z')",
                rusqlite::params![9000 + i, uid],
            )
            .unwrap();
        }
        uid
    }

    /// The v1.x route answered hardcoded zeros — the real snapshot carries
    /// live per-agent counts, the capacity model and the method notes.
    #[tokio::test]
    async fn workload_route_returns_real_counts() {
        let state = make_state();
        let ada = seed_agent_with_open(&state, 501, "Ada", 2);
        seed_agent_with_open(&state, 502, "Grace", 0);
        // One unassigned conversation.
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO conversations
                    (remote_id, number, status, mailbox_id, customer_id, created_at)
                 VALUES (9999, 9999, 'active', 1, 3001, '2026-10-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        let response = workload(State(state.clone())).await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["unassigned_work"], json!(1));
        let agents = body["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 2);
        let ada_row = agents
            .iter()
            .find(|a| a["user_local_id"] == json!(ada))
            .unwrap();
        assert_eq!(ada_row["display_name"], json!("Ada Agent"));
        assert_eq!(ada_row["open_workload"], json!(2));
        assert_eq!(ada_row["weighted_load"], json!(2.0));
        assert_eq!(ada_row["capacity"], json!(25));
        assert_eq!(ada_row["pressure"], json!(0.08));
        assert_eq!(ada_row["availability"]["source"], json!("unknown"));
        assert_eq!(body["capacity_model"]["default_max_open"], json!(25));
        assert_eq!(body["method_notes"].as_array().unwrap().len(), 4);
    }

    /// Capacity PUT: the reference 422 (first issue as message, all in
    /// detail) and the `{ saved: true, capacity_model }` round-trip.
    #[tokio::test]
    async fn capacity_route_validates_and_round_trips() {
        let state = make_state();
        let agent = seed_agent_with_open(&state, 701, "Cap", 0);
        let response =
            set_capacity(State(state.clone()), Json(json!({ "default_max_open": 0 }))).await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], json!("ValidationError"));
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("default_max_open"));
        assert!(body["detail"].as_array().unwrap().len() >= 3);

        let response = set_capacity(
            State(state.clone()),
            Json(json!({
                "default_max_open": 15,
                "per_user_max": { "1": 5 },
                "weights": { "urgent": 4, "sla": 2, "waiting": 1.5, "open": 1 }
            })),
        )
        .await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["saved"], json!(true));
        assert_eq!(body["capacity_model"]["default_max_open"], json!(15));
        assert_eq!(body["capacity_model"]["per_user_max"]["1"], json!(5));
        // The stored model drives the next snapshot's capacity (the agent's
        // local id is 1 on a fresh database).
        let response = workload(State(state.clone())).await;
        let (_, body) = body_json(response.into_response()).await;
        let row = body["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["user_local_id"] == json!(agent))
            .unwrap();
        assert_eq!(row["capacity"], json!(5));
    }

    /// Suggested assignees: real recommendations with reasoning, `limit`
    /// clamps, garbage numerics fall back to 10 (never a 500).
    #[tokio::test]
    async fn suggested_assignees_route_recommends_with_reasoning() {
        let state = make_state();
        let quiet = seed_agent_with_open(&state, 601, "Quiet", 0);
        seed_agent_with_open(&state, 602, "Busy", 4);
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO conversations
                (remote_id, number, status, mailbox_id, customer_id, subject,
                 supportos_priority, customer_waiting_since, created_at)
             VALUES (8001, 8001, 'active', 1, 3001, 'Broken login',
                     'urgent', '2026-10-04T00:00:00Z', '2026-10-04T00:00:00Z')",
            [],
        )
        .unwrap();
        drop(conn);
        let response = suggested_assignees(
            State(state.clone()),
            Query(HashMap::from([("limit".to_string(), "abc".to_string())])),
        )
        .await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let suggestions = body["suggestions"].as_array().unwrap();
        assert_eq!(suggestions.len(), 1);
        let s = &suggestions[0];
        assert_eq!(s["subject"], json!("Broken login"));
        assert_eq!(s["supportos_priority"], json!("urgent"));
        assert_eq!(s["suggested_user_local_id"], json!(quiet));
        assert_eq!(s["suggested_display_name"], json!("Quiet Agent"));
        assert!(s["reason"]
            .as_str()
            .unwrap()
            .contains("Lowest resulting pressure"));
        // limit=1 clamps.
        let response = suggested_assignees(
            State(state.clone()),
            Query(HashMap::from([("limit".to_string(), "1".to_string())])),
        )
        .await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["suggestions"].as_array().unwrap().len(), 1);
    }
}
