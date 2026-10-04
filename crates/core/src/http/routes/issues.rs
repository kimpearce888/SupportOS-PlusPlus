//! Issues routes — mirrors src/server/routes/issues.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::super::server::AppState;

/// GET /api/issues/clusters
pub async fn list_clusters(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let clusters: Vec<Value> = conn
        .prepare("SELECT id, name, status, created_at FROM issue_clusters ORDER BY id DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        })
        .unwrap_or_default();
    Json(json!({"clusters": clusters}))
}

/// GET /api/issues/sla-alerts
///
/// v1.5.0: business-hours-aware SLA alerts for the Issue Radar (reference
/// routes/issues.ts:9 — `ctx.sla.slaAlerts()` verbatim).
///
/// Reference response shape:
/// ```json
/// {
///   "generated_at": "...",
///   "total_breached": N, "total_at_risk": N,
///   "alerts": [ { conversation_id, number, subject, status, mailbox_id,
///                 mailbox_name, assignee_local_id, state,
///                 waited_business_min, target_min, target_kind,
///                 overdue_business_min, since } ],
///   "per_mailbox": [ { mailbox_id, mailbox_name, breached, at_risk, monitored } ],
///   "unconfigured_mailboxes": ["..."],
///   "note": "Alerts measure BUSINESS minutes ..."
/// }
/// ```
pub async fn sla_alerts(State(state): State<AppState>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::sla::sla_alerts(&conn) {
        Ok(alerts) => (
            StatusCode::OK,
            Json(serde_json::to_value(&alerts).unwrap_or(Value::Null)),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /api/issues/clusters/:id
pub async fn get_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let row = conn.query_row(
        "SELECT id, name, status, created_at FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
        |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "created_at": r.get::<_, String>(3)?,
            }))
        },
    );
    match row {
        Ok(v) => Json(v),
        Err(_) => Json(json!({"error": "Cluster not found"})),
    }
}

/// DELETE /api/issues/clusters/:id
pub async fn delete_cluster(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM issue_clusters WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known
pub async fn list_known(State(state): State<AppState>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let issues = crate::intelligence_features::list_known_issues(&conn, None).unwrap_or_default();
    let items: Vec<Value> = issues
        .iter()
        .filter_map(|i| serde_json::to_value(i).ok())
        .collect();
    Json(json!({"known_issues": items}))
}

/// POST /api/issues/known
pub async fn create_known(State(state): State<AppState>, Json(body): Json<Value>) -> Json<Value> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT INTO known_issues (name, status, description, created_at, updated_at) VALUES (?1, 'active', ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        rusqlite::params![name, description],
    );
    Json(json!({"ok": true}))
}

/// GET /api/issues/known/:id
pub async fn get_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::intelligence_features::list_known_issues(&conn, Some("active")) {
        Ok(issues) => {
            if let Some(issue) = issues.iter().find(|i| i.id == Some(id)) {
                Json(serde_json::to_value(issue).unwrap_or(json!({})))
            } else {
                Json(json!({"error": "Known issue not found"}))
            }
        }
        Err(_) => Json(json!({"error": "Failed to list known issues"})),
    }
}

/// PATCH /api/issues/known/:id
pub async fn update_known(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET name = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![name, id]);
    }
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        let _ = conn.execute("UPDATE known_issues SET status = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2", rusqlite::params![status, id]);
    }
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id
pub async fn delete_known(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM known_issues WHERE id = ?1",
        rusqlite::params![id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/link/:conversationId
pub async fn link_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "INSERT OR IGNORE INTO known_issue_links (known_issue_id, conversation_id, link_type) VALUES (?1, ?2, 'manual')",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// DELETE /api/issues/known/:id/link/:conversationId
pub async fn unlink_known(
    State(state): State<AppState>,
    Path((id, conversation_id)): Path<(i64, i64)>,
) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    let _ = conn.execute(
        "DELETE FROM known_issue_links WHERE known_issue_id = ?1 AND conversation_id = ?2",
        rusqlite::params![id, conversation_id],
    );
    Json(json!({"ok": true}))
}

/// POST /api/issues/known/:id/refs
pub async fn add_ref(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Json<Value> {
    Json(json!({"ok": true, "id": id}))
}

/// GET /api/issues/cases
pub async fn list_cases(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"cases": []}))
}

/// POST /api/issues/cases/from-conversation/:conversationId
pub async fn case_from_conversation(
    State(state): State<AppState>,
    Path(conversation_id): Path<i64>,
) -> Json<Value> {
    Json(json!({"ok": true, "conversationId": conversation_id}))
}

/// GET /api/issues/known/:id/impact — known-issue impact through the ONE
/// shared issue-impact compute (reference incidents.ts:277-285 —
/// `ctx.issueImpact.forKnownIssue(id)`): derived distinct-entity counts,
/// first/last seen, 7-day growth vs the previous 7, trend, top-10
/// inbox/tag/product breakdowns and the honest-labeling note. 404 when
/// the known issue does not exist.
pub async fn known_impact(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    match crate::issue_impact::for_known_issue(&conn, id) {
        Some(impact) => (StatusCode::OK, Json(json!({"impact": impact}))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "statusCode": 404,
                "error": "NotFound",
                "message": "Known issue not found."
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::{IntoResponse, Response};
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

    fn seed_waiting_conversation(state: &AppState, mins_waiting: i64) {
        let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO mailboxes (id, remote_id, name) VALUES (1, 201, 'Support')",
            [],
        )
        .unwrap();
        let ts = chrono::Utc::now().timestamp_millis() - mins_waiting * 60_000;
        let at = chrono::TimeZone::timestamp_millis_opt(&chrono::Utc, ts)
            .single()
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        conn.execute(
            "INSERT INTO conversations (remote_id, number, mailbox_id, customer_id, status, created_at, updated_at)
             VALUES (101, 101, 1, 3001, 'active', ?1, ?1)",
            rusqlite::params![at],
        )
        .unwrap();
        let id: i64 = conn
            .query_row(
                "SELECT id FROM conversations WHERE remote_id = 101",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO conversation_threads (conversation_id, thread_type, state, body, actor_type, created_at)
             VALUES (?1, 'customer', 'published', 'b', 'customer', ?2)",
            rusqlite::params![id, at],
        )
        .unwrap();
    }

    /// Reference e2e (v15): honest unconfigured state — zero totals, the
    /// mailbox listed as unconfigured, the note mentions BUSINESS minutes,
    /// and the full route shape answers 200.
    #[tokio::test]
    async fn sla_alerts_route_honest_unconfigured_state() {
        let state = make_state();
        seed_waiting_conversation(&state, 300);
        let response = sla_alerts(State(state)).await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_breached"], json!(0));
        assert_eq!(body["total_at_risk"], json!(0));
        assert_eq!(body["alerts"], json!([]));
        assert_eq!(body["unconfigured_mailboxes"], json!(["Support"]));
        assert!(body["note"].as_str().unwrap().contains("BUSINESS minutes"));
        assert!(
            body["generated_at"].as_str().unwrap().ends_with('Z'),
            "toISOString format: {}",
            body["generated_at"]
        );
        assert!(body["per_mailbox"].as_array().is_some());
    }

    /// Reference e2e (v15): after configuring 24/7 hours + a 60-minute
    /// first-response target, a conversation waiting 2h shows up breached
    /// with the full alert row shape.
    #[tokio::test]
    async fn sla_alerts_route_live_breach_after_configuration() {
        let state = make_state();
        seed_waiting_conversation(&state, 120);
        {
            let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO mailbox_business_hours
                    (mailbox_local_id, timezone, days, start_minute, end_minute, first_response_target_min, resolution_target_min, updated_at)
                 VALUES (1, 'UTC', '[0,1,2,3,4,5,6]', 0, 1440, 60, 480, '2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
        }
        let response = sla_alerts(State(state)).await;
        let (status, body) = body_json(response.into_response()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_breached"], json!(1));
        assert_eq!(body["total_at_risk"], json!(0));
        let alert = &body["alerts"][0];
        assert_eq!(alert["state"], json!("breached"));
        assert_eq!(alert["target_kind"], json!("first_response"));
        assert_eq!(alert["target_min"], json!(60));
        assert_eq!(alert["waited_business_min"], json!(120));
        assert_eq!(alert["overdue_business_min"], json!(60));
        assert_eq!(alert["mailbox_name"], json!("Support"));
        assert_eq!(body["unconfigured_mailboxes"], json!([]));
    }
}
