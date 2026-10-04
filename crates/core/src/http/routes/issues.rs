//! Issues routes — mirrors src/server/routes/issues.ts

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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

/// GET /api/issues/known/:id/impact — known-issue impact (reference
/// incidents.ts:277 — `ctx.issueImpact.forKnownIssue(id)`).
///
/// Impact = conversations / distinct customers / organizations, first + last
/// seen (remote_created_at), open/closed/waiting counts, 7-day growth vs the
/// previous 7 days, trend classification, top-10 mailbox breakdown.
pub async fn known_impact(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let conn = state.conn.lock().unwrap_or_else(|p| p.into_inner());
    // 404 when the known issue itself does not exist.
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM known_issues WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        return Json(json!({
            "statusCode": 404,
            "error": "NotFound",
            "message": "Known issue not found."
        }));
    }
    // Base counts over linked, non-deleted conversations.
    let counts_ok = conn.query_row(
        "SELECT COUNT(*),
                COUNT(DISTINCT c.customer_id),
                MIN(c.remote_created_at),
                MAX(c.remote_created_at),
                SUM(CASE WHEN c.status = 'active' THEN 1 ELSE 0 END),
                SUM(CASE WHEN c.status = 'closed' THEN 1 ELSE 0 END),
                SUM(CASE WHEN c.customer_waiting_since IS NOT NULL AND c.status = 'active' THEN 1 ELSE 0 END)
         FROM conversations c
         WHERE c.deleted_at IS NULL AND c.id IN (
            SELECT l.conversation_id FROM known_issue_conversations l
            WHERE l.known_issue_id = ?1
         )",
        rusqlite::params![id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                r.get::<_, Option<i64>>(6)?.unwrap_or(0),
            ))
        },
    );
    let (conversations, customers, first_seen, last_seen, open_count, closed_count, waiting_count) =
        match counts_ok {
            Ok(v) => v,
            Err(_) => (0, 0, None, None, 0, 0, 0),
        };
    // Distinct organizations among linked conversations' customers.
    let organizations: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT cu.organization_id)
             FROM conversations c JOIN customers cu ON cu.id = c.customer_id
             WHERE c.deleted_at IS NULL AND cu.organization_id IS NOT NULL
               AND c.id IN (
                 SELECT l.conversation_id FROM known_issue_conversations l
                 WHERE l.known_issue_id = ?1
               )",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    // Growth: started in the last 7 days vs the previous 7.
    let conv_in = "|LINK|";
    let _ = conv_in;
    let recent: i64 = linked_count_since(&conn, id, "-7 days").unwrap_or(0);
    let previous: i64 = linked_count_between(&conn, id, "-14 days", "-7 days").unwrap_or(0);
    let recent14: i64 = linked_count_since(&conn, id, "-14 days").unwrap_or(0);
    let prior14: i64 = linked_count_between(&conn, id, "-28 days", "-14 days").unwrap_or(0);
    let growth = if previous > 0 {
        json!({ "window_days": 7, "recent": recent, "previous": previous,
                "ratio": (recent as f64) / (previous as f64),
                "direction": if (recent as f64) / (previous as f64) >= 1.3 { "rising" }
                             else if (recent as f64) / (previous as f64) <= 0.7 { "falling" } else { "flat" } })
    } else if recent > 0 {
        json!({ "window_days": 7, "recent": recent, "previous": previous, "ratio": null, "direction": "rising" })
    } else {
        json!({ "window_days": 7, "recent": recent, "previous": previous, "ratio": 0.0, "direction": "unknown" })
    };
    // Trend: new / rising / falling / stable (reference impact.ts compute).
    let trend = if conversations == 0 {
        "unknown".to_string()
    } else if first_seen
        .as_deref()
        .map(|fs| {
            conn.query_row(
                "SELECT julianday(?1) >= julianday('now', '-7 days')",
                rusqlite::params![fs],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v == 1)
            .unwrap_or(false)
        })
        .unwrap_or(false)
    {
        "new".to_string()
    } else if prior14 == 0 && recent14 >= 1 {
        "rising".to_string()
    } else if prior14 > 0 && recent14 < (prior14 as f64 * 0.7) as i64 {
        "falling".to_string()
    } else {
        "stable".to_string()
    };
    // Top-10 mailbox breakdown.
    let mut inboxes = Vec::new();
    let _ = conn.prepare(
        "SELECT (SELECT m.name FROM mailboxes m WHERE m.id = c.mailbox_local_id) AS mailbox, COUNT(*) AS n
         FROM conversations c
         WHERE c.deleted_at IS NULL AND c.id IN (
            SELECT l.conversation_id FROM known_issue_conversations l WHERE l.known_issue_id = ?1
         )
         GROUP BY c.mailbox_local_id ORDER BY n DESC LIMIT 10",
    )
    .and_then(|mut stmt| {
        let rows = stmt.query_map(rusqlite::params![id], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows.flatten() {
            inboxes.push(json!({ "mailbox": row.0, "conversations": row.1 }));
        }
        Ok(())
    });
    Json(json!({
        "impact": {
            "conversations": conversations,
            "customers": customers,
            "organizations": organizations,
            "first_seen": first_seen,
            "last_seen": last_seen,
            "open_count": open_count,
            "closed_count": closed_count,
            "waiting_count": waiting_count,
            "growth_rate_7d": growth,
            "trend": trend,
            "inboxes": inboxes
        }
    }))
}

/// COUNT of linked conversations created at or after `julianday('now', since)`.
fn linked_count_since(conn: &rusqlite::Connection, id: i64, since: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations c
             WHERE c.deleted_at IS NULL
               AND julianday(c.remote_created_at) >= julianday('now', '{since}')
               AND c.id IN (SELECT l.conversation_id FROM known_issue_conversations l WHERE l.known_issue_id = ?1)"
        ),
        rusqlite::params![id],
        |r| r.get(0),
    )
}

/// COUNT of linked conversations created in [from, to) relative to now.
fn linked_count_between(
    conn: &rusqlite::Connection,
    id: i64,
    from: &str,
    to: &str,
) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations c
             WHERE c.deleted_at IS NULL
               AND julianday(c.remote_created_at) >= julianday('now', '{from}')
               AND julianday(c.remote_created_at) < julianday('now', '{to}')
               AND c.id IN (SELECT l.conversation_id FROM known_issue_conversations l WHERE l.known_issue_id = ?1)"
        ),
        rusqlite::params![id],
        |r| r.get(0),
    )
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
